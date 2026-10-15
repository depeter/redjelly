//! Everything the user can ask for, independent of the input device.

use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    // Navigation (browse screens and dialogs)
    NavUp,
    NavDown,
    NavLeft,
    NavRight,
    Activate,
    Back,
    NextRegion,
    PrevRegion,
    Search,
    ContextMenu,
    GoHome,
    SwitchUser,
    Jobs,
    Settings,

    // Global
    ToggleFullscreen,
    ShowKeymap,
    ShowStats,
    Quit,

    // Player
    PlayPause,
    Stop,
    Seek(f64),
    SeekExact(f64),
    SeekPercent(f64),
    FrameStep,
    FrameBack,
    Volume(f64),
    Mute,
    Chapter(i32),
    NextEpisode,
    PreviousEpisode,
    Speed(f64),
    SpeedReset,
    CycleAudio,
    CycleSubtitle,
    CycleSubtitleBack,
    ToggleSubtitles,
    SubtitlePanel,
    /// Milliseconds.
    SubDelay(i32),
    SubAutoSync,
    SkipSegment,
    CycleShader,
    CycleToneMapping,
    OsdMenu,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseActionError {
    #[error("unknown action `{0}`")]
    Unknown(String),
    #[error("action `{0}` needs an argument, e.g. `{0}:5`")]
    MissingArg(String),
    #[error("action `{0}` takes no argument")]
    UnexpectedArg(String),
    #[error("bad argument `{1}` for action `{0}`")]
    BadArg(String, String),
}

impl FromStr for Action {
    type Err = ParseActionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (name, arg) = match s.trim().split_once(':') {
            Some((n, a)) => (n.trim(), Some(a.trim())),
            None => (s.trim(), None),
        };
        let f = || -> Result<f64, ParseActionError> {
            let a = arg.ok_or_else(|| ParseActionError::MissingArg(name.into()))?;
            a.parse().map_err(|_| ParseActionError::BadArg(name.into(), a.into()))
        };
        let i = || -> Result<i32, ParseActionError> {
            let a = arg.ok_or_else(|| ParseActionError::MissingArg(name.into()))?;
            a.parse().map_err(|_| ParseActionError::BadArg(name.into(), a.into()))
        };

        let with_arg = match name {
            "seek" => Some(Action::Seek(f()?)),
            "seek_exact" => Some(Action::SeekExact(f()?)),
            "seek_percent" => Some(Action::SeekPercent(f()?)),
            "volume" => Some(Action::Volume(f()?)),
            "speed" => Some(Action::Speed(f()?)),
            "chapter" => Some(Action::Chapter(i()?)),
            "sub_delay" => Some(Action::SubDelay(i()?)),
            _ => None,
        };
        if let Some(a) = with_arg {
            return Ok(a);
        }
        if arg.is_some() {
            return Err(ParseActionError::UnexpectedArg(name.into()));
        }

        Ok(match name {
            "nav_up" => Action::NavUp,
            "nav_down" => Action::NavDown,
            "nav_left" => Action::NavLeft,
            "nav_right" => Action::NavRight,
            "activate" => Action::Activate,
            "back" => Action::Back,
            "next_region" => Action::NextRegion,
            "prev_region" => Action::PrevRegion,
            "search" => Action::Search,
            "context_menu" => Action::ContextMenu,
            "go_home" => Action::GoHome,
            "switch_user" => Action::SwitchUser,
            "jobs" => Action::Jobs,
            "settings" => Action::Settings,
            "cycle_tone_mapping" => Action::CycleToneMapping,
            "toggle_fullscreen" => Action::ToggleFullscreen,
            "show_keymap" => Action::ShowKeymap,
            "show_stats" => Action::ShowStats,
            "quit" => Action::Quit,
            "play_pause" => Action::PlayPause,
            "stop" => Action::Stop,
            "frame_step" => Action::FrameStep,
            "frame_back" => Action::FrameBack,
            "mute" => Action::Mute,
            "next_episode" => Action::NextEpisode,
            "previous_episode" => Action::PreviousEpisode,
            "speed_reset" => Action::SpeedReset,
            "cycle_audio" => Action::CycleAudio,
            "cycle_subtitle" => Action::CycleSubtitle,
            "cycle_subtitle_back" => Action::CycleSubtitleBack,
            "toggle_subtitles" => Action::ToggleSubtitles,
            "subtitle_panel" => Action::SubtitlePanel,
            "sub_auto_sync" => Action::SubAutoSync,
            "skip_segment" => Action::SkipSegment,
            "cycle_shader" => Action::CycleShader,
            "osd_menu" => Action::OsdMenu,
            _ => return Err(ParseActionError::Unknown(name.into())),
        })
    }
}

impl fmt::Display for Action {
    /// Human-readable label for the cheat sheet and keymap editor.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Seek(s) | Action::SeekExact(s) => write!(f, "Seek {s:+} s"),
            Action::SeekPercent(p) => write!(f, "Seek to {p}%"),
            Action::Volume(v) => write!(f, "Volume {v:+}"),
            Action::Speed(v) => write!(f, "Speed {v:+}"),
            Action::Chapter(c) => write!(f, "Chapter {c:+}"),
            Action::SubDelay(ms) => write!(f, "Subtitle delay {ms:+} ms"),
            Action::NavUp => f.write_str("Move up"),
            Action::NavDown => f.write_str("Move down"),
            Action::NavLeft => f.write_str("Move left"),
            Action::NavRight => f.write_str("Move right"),
            Action::Activate => f.write_str("Open / play"),
            Action::NextRegion => f.write_str("Next row"),
            Action::PrevRegion => f.write_str("Previous row"),
            Action::ContextMenu => f.write_str("Context menu"),
            Action::GoHome => f.write_str("Home screen"),
            Action::SwitchUser => f.write_str("Switch user"),
            Action::Jobs => f.write_str("AI subtitle jobs"),
            Action::Settings => f.write_str("Settings"),
            Action::CycleToneMapping => f.write_str("Next HDR tone-mapping preset"),
            Action::Stop => f.write_str("Stop"),
            Action::Back => f.write_str("Back"),
            Action::Search => f.write_str("Search"),
            Action::Quit => f.write_str("Quit"),
            Action::Mute => f.write_str("Mute"),
            Action::ToggleFullscreen => f.write_str("Fullscreen"),
            Action::ShowKeymap => f.write_str("This key list"),
            Action::ShowStats => f.write_str("Stats overlay"),
            Action::PlayPause => f.write_str("Play / pause"),
            Action::FrameStep => f.write_str("Next frame"),
            Action::FrameBack => f.write_str("Previous frame"),
            Action::NextEpisode => f.write_str("Next episode"),
            Action::PreviousEpisode => f.write_str("Previous episode"),
            Action::SpeedReset => f.write_str("Normal speed"),
            Action::CycleAudio => f.write_str("Next audio track"),
            Action::CycleSubtitle => f.write_str("Next subtitle"),
            Action::CycleSubtitleBack => f.write_str("Previous subtitle"),
            Action::ToggleSubtitles => f.write_str("Show/hide subtitles"),
            Action::SubtitlePanel => f.write_str("Subtitle panel"),
            Action::SubAutoSync => f.write_str("Auto-sync subtitles"),
            Action::SkipSegment => f.write_str("Skip intro/credits"),
            Action::CycleShader => f.write_str("Next shader preset"),
            Action::OsdMenu => f.write_str("Menu"),
        }
    }
}

impl Action {
    /// The keymap-file spelling, inverse of `FromStr`.
    pub fn config_name(&self) -> String {
        let arg = |name: &str, v: String| format!("{name}:{v}");
        match self {
            Action::Seek(v) => arg("seek", v.to_string()),
            Action::SeekExact(v) => arg("seek_exact", v.to_string()),
            Action::SeekPercent(v) => arg("seek_percent", v.to_string()),
            Action::Volume(v) => arg("volume", v.to_string()),
            Action::Speed(v) => arg("speed", v.to_string()),
            Action::Chapter(v) => arg("chapter", v.to_string()),
            Action::SubDelay(v) => arg("sub_delay", v.to_string()),
            other => {
                // Unit variants: CamelCase → snake_case.
                let mut out = String::new();
                for (i, c) in format!("{other:?}").chars().enumerate() {
                    if c.is_ascii_uppercase() {
                        if i > 0 {
                            out.push('_');
                        }
                        out.push(c.to_ascii_lowercase());
                    } else {
                        out.push(c);
                    }
                }
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_args() {
        assert_eq!("seek:-5".parse(), Ok(Action::Seek(-5.0)));
        assert_eq!("sub_delay: 100".parse(), Ok(Action::SubDelay(100)));
        assert_eq!("play_pause".parse(), Ok(Action::PlayPause));
    }

    #[test]
    fn config_names_round_trip() {
        for a in [Action::PlayPause, Action::Seek(-5.0), Action::SubDelay(100), Action::CycleToneMapping, Action::Speed(0.1)] {
            assert_eq!(a.config_name().parse::<Action>(), Ok(a), "{}", a.config_name());
        }
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!("seek".parse::<Action>(), Err(ParseActionError::MissingArg("seek".into())));
        assert_eq!("mute:1".parse::<Action>(), Err(ParseActionError::UnexpectedArg("mute".into())));
        assert!(matches!("nope".parse::<Action>(), Err(ParseActionError::Unknown(_))));
    }
}
