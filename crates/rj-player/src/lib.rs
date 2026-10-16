//! libmpv wrapper: owns the mpv core, applies [`Action`]s and settings.
//!
//! Video output goes through mpv's render API (`vo=libmpv`); the app creates
//! the OpenGL render context against Slint's GL context (SPEC §2.1, spike M0).

pub mod extract;
pub mod render;
pub mod track_map;

use libmpv2::Mpv;
use rj_core::config::{AssOverride, Config};
use rj_input::Action;

pub use libmpv2;
pub use libmpv2::Error as MpvError;

/// One entry of mpv's `track-list`.
#[derive(Debug, Clone)]
pub struct MpvTrack {
    pub id: i64,
    pub kind: String,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub codec: Option<String>,
    pub default: bool,
    pub forced: bool,
    pub hearing_impaired: bool,
    pub external: bool,
    pub channels: u32,
}

impl MpvTrack {
    pub fn to_audio(&self) -> rj_core::tracks::AudioTrack {
        rj_core::tracks::AudioTrack {
            index: self.id as u32,
            lang: self.lang.as_deref().and_then(rj_core::lang::Lang::parse),
            channels: self.channels,
            is_default: self.default,
        }
    }

    pub fn to_subtitle(&self) -> rj_core::tracks::SubtitleTrack {
        use rj_core::tracks::SubKind;
        let image = matches!(
            self.codec.as_deref(),
            Some("hdmv_pgs_subtitle" | "dvd_subtitle" | "dvb_subtitle" | "pgs" | "vobsub")
        );
        let title = self.title.as_deref().unwrap_or("").to_ascii_lowercase();
        rj_core::tracks::SubtitleTrack {
            index: self.id as u32,
            lang: self.lang.as_deref().and_then(rj_core::lang::Lang::parse),
            kind: if image { SubKind::Image } else { SubKind::Text },
            forced: self.forced || title.contains("forced"),
            hearing_impaired: self.hearing_impaired || title.contains("sdh"),
            is_default: self.default,
        }
    }
}

pub struct Player {
    /// Leaked on purpose: the render context borrows the core for its whole
    /// life, and there is exactly one player per process.
    mpv: &'static Mpv,
}

impl Player {
    /// `log_file`: mpv's own verbose log, invaluable for playback bug reports.
    pub fn new(cfg: &Config, log_file: Option<&std::path::Path>) -> Result<Self, MpvError> {
        let mpv = Mpv::with_initializer(|init| {
            init.set_option("vo", "libmpv")?;
            init.set_option("hwdec", cfg.playback.hwdec.as_str())?;
            init.set_option("keep-open", "no")?;
            if let Some(path) = log_file {
                init.set_option("log-file", path.to_string_lossy().as_ref())?;
            }
            // We render our own OSD and handle all input.
            init.set_option("osc", false)?;
            init.set_option("input-default-bindings", false)?;
            init.set_option("input-vo-keyboard", false)?;
            init.set_option("cache", "yes")?;
            init.set_option("demuxer-max-bytes", "512MiB")?;
            // libmpv starts from mpv's fast defaults and reads no mpv.conf,
            // so ask for what a tuned mpv uses: better scalers, debanding.
            // (`gpu-hq` is the pre-0.37 name.)
            if init.set_option("profile", "high-quality").is_err() {
                let _ = init.set_option("profile", "gpu-hq");
            }
            // Slint's framebuffer is 8-bit: dither, or HDR→SDR gradients band.
            init.set_option("dither-depth", "auto")?;
            Ok(())
        })?;
        let player = Self { mpv: Box::leak(Box::new(mpv)) };
        player.apply_subtitle_style(cfg)?;
        Ok(player)
    }

    pub fn mpv(&self) -> &'static Mpv {
        self.mpv
    }

    pub fn apply_subtitle_style(&self, cfg: &Config) -> Result<(), MpvError> {
        let s = &cfg.subtitles;
        self.mpv.set_property("sub-font", s.font.as_str())?;
        self.mpv.set_property("sub-font-size", i64::from(s.font_size))?;
        self.mpv.set_property("sub-color", s.color.as_str())?;
        self.mpv.set_property("sub-border-size", f64::from(s.border_size))?;
        self.mpv.set_property("sub-margin-y", i64::from(s.margin_y))?;
        let ass = match s.ass_override {
            AssOverride::No => "no",
            AssOverride::Scale => "scale",
            AssOverride::Force => "force",
        };
        self.mpv.set_property("sub-ass-override", ass)
    }

    /// Loads a stream. `auth_header` is sent with every HTTP request mpv makes.
    pub fn load(&self, url: &str, auth_header: &str, start_secs: f64) -> Result<(), MpvError> {
        self.mpv
            .set_property("http-header-fields", format!("Authorization: {auth_header}"))?;
        self.mpv.command("loadfile", &[url, "replace", "-1", &format!("start={start_secs}")])
    }

    /// Adds an external subtitle; returns mpv's track id for it.
    pub fn add_subtitle(&self, url: &str, title: &str, lang: &str) -> Result<i64, MpvError> {
        self.mpv.command("sub-add", &[url, "auto", title, lang])?;
        // The new track is last in the list and selected-or-not per "auto".
        let count: i64 = self.mpv.get_property("track-list/count")?;
        self.mpv.get_property(&format!("track-list/{}/id", count - 1))
    }

    pub fn set_audio(&self, mpv_id: i64) -> Result<(), MpvError> {
        self.mpv.set_property("aid", mpv_id)
    }

    pub fn set_subtitle(&self, mpv_id: Option<i64>) -> Result<(), MpvError> {
        match mpv_id {
            Some(id) => self.mpv.set_property("sid", id),
            None => self.mpv.set_property("sid", "no"),
        }
    }

    /// Reads mpv's current track list (valid after `FileLoaded`).
    pub fn tracks(&self) -> Vec<MpvTrack> {
        let m = self.mpv;
        let count: i64 = m.get_property("track-list/count").unwrap_or(0);
        (0..count)
            .filter_map(|i| {
                let p = |k: &str| format!("track-list/{i}/{k}");
                let s = |k: &str| m.get_property::<String>(&p(k)).ok().filter(|v| !v.is_empty());
                let b = |k: &str| m.get_property::<bool>(&p(k)).unwrap_or(false);
                Some(MpvTrack {
                    id: m.get_property(&p("id")).ok()?,
                    kind: s("type")?,
                    lang: s("lang"),
                    title: s("title"),
                    codec: s("codec"),
                    default: b("default"),
                    forced: b("forced"),
                    hearing_impaired: b("hearing-impaired"),
                    external: b("external"),
                    channels: m.get_property::<i64>(&p("demux-channel-count")).unwrap_or(0) as u32,
                })
            })
            .collect()
    }

    /// Multi-line technical overlay (I key): decoder, hwdec, HDR, drops.
    pub fn stats(&self) -> String {
        let m = self.mpv;
        let g = |k: &str| m.get_property::<String>(k).unwrap_or_else(|_| "-".into());
        format!(
            "video   {} {}x{} @ {} fps\nhwdec   {}\nvo      {}\ncolor   {} / {} / peak {}\naudio   {} {} ch\ndrops   {} decoder, {} vo\ncache   {} s",
            g("video-codec"),
            g("video-params/w"),
            g("video-params/h"),
            g("container-fps"),
            g("hwdec-current"),
            g("current-vo"),
            g("video-params/primaries"),
            g("video-params/gamma"),
            g("video-params/sig-peak"),
            g("audio-codec-name"),
            g("audio-params/channel-count"),
            g("decoder-frame-drop-count"),
            g("frame-drop-count"),
            g("demuxer-cache-duration"),
        )
    }

    pub fn duration(&self) -> f64 {
        self.mpv.get_property("duration").unwrap_or(0.0)
    }

    pub fn idle(&self) -> bool {
        self.mpv.get_property("idle-active").unwrap_or(true)
    }

    pub fn position(&self) -> f64 {
        self.mpv.get_property("time-pos").unwrap_or(0.0)
    }

    pub fn paused(&self) -> bool {
        self.mpv.get_property("pause").unwrap_or(false)
    }

    /// Applies a player action. Returns the action back if it needs app-level
    /// handling (episodes, panels, skip segment…).
    pub fn apply(&self, action: Action) -> Result<Option<Action>, MpvError> {
        let m = self.mpv;
        match action {
            Action::PlayPause => m.command("cycle", &["pause"])?,
            Action::Seek(s) => m.command("seek", &[&s.to_string(), "relative"])?,
            Action::SeekExact(s) => m.command("seek", &[&s.to_string(), "relative+exact"])?,
            Action::SeekPercent(p) => m.command("seek", &[&p.to_string(), "absolute-percent"])?,
            Action::FrameStep => m.command("frame-step", &[])?,
            Action::FrameBack => m.command("frame-back-step", &[])?,
            Action::Volume(v) => m.command("add", &["volume", &v.to_string()])?,
            Action::Mute => m.command("cycle", &["mute"])?,
            Action::Chapter(c) => m.command("add", &["chapter", &c.to_string()])?,
            Action::Speed(v) => m.command("add", &["speed", &v.to_string()])?,
            Action::SpeedReset => m.set_property("speed", 1.0)?,
            Action::CycleAudio => m.command("cycle", &["audio"])?,
            Action::CycleSubtitle => m.command("cycle", &["sub"])?,
            Action::CycleSubtitleBack => m.command("cycle", &["sub", "down"])?,
            Action::ToggleSubtitles => m.command("cycle", &["sub-visibility"])?,
            Action::SubDelay(ms) => m.command("add", &["sub-delay", &(f64::from(ms) / 1000.0).to_string()])?,
            Action::Stop => m.command("stop", &[])?,
            other => return Ok(Some(other)),
        }
        Ok(None)
    }
}
