//! OS media integration: SMTC on Windows (media keys, volume flyout, lock
//! screen), MPRIS on Linux. Events are mapped straight to [`Action`]s.

use std::time::Duration;

use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig, SeekDirection};

use crate::Action;

pub struct NowPlaying<'a> {
    pub title: &'a str,
    pub subtitle: Option<&'a str>,
    pub cover_url: Option<&'a str>,
    pub duration: Option<Duration>,
}

pub struct OsMediaControls {
    controls: MediaControls,
}

impl OsMediaControls {
    /// `hwnd` is the main window handle on Windows; ignored elsewhere.
    pub fn new(
        hwnd: Option<*mut std::ffi::c_void>,
        on_action: impl Fn(Action) + Send + 'static,
    ) -> Result<Self, souvlaki::Error> {
        let mut controls = MediaControls::new(PlatformConfig {
            dbus_name: "redjelly",
            display_name: "redjelly",
            hwnd,
        })?;
        controls.attach(move |ev| {
            if let Some(action) = map_event(ev) {
                on_action(action);
            }
        })?;
        Ok(Self { controls })
    }

    pub fn set_now_playing(&mut self, np: &NowPlaying<'_>) -> Result<(), souvlaki::Error> {
        self.controls.set_metadata(MediaMetadata {
            title: Some(np.title),
            album: np.subtitle,
            artist: None,
            cover_url: np.cover_url,
            duration: np.duration,
        })
    }

    pub fn set_state(&mut self, playing: Option<bool>, position: Duration) -> Result<(), souvlaki::Error> {
        let progress = Some(MediaPosition(position));
        self.controls.set_playback(match playing {
            Some(true) => MediaPlayback::Playing { progress },
            Some(false) => MediaPlayback::Paused { progress },
            None => MediaPlayback::Stopped,
        })
    }
}

fn map_event(ev: MediaControlEvent) -> Option<Action> {
    Some(match ev {
        MediaControlEvent::Play | MediaControlEvent::Pause | MediaControlEvent::Toggle => Action::PlayPause,
        MediaControlEvent::Next => Action::NextEpisode,
        MediaControlEvent::Previous => Action::PreviousEpisode,
        MediaControlEvent::Stop => Action::Stop,
        MediaControlEvent::Seek(SeekDirection::Forward) => Action::Seek(10.0),
        MediaControlEvent::Seek(SeekDirection::Backward) => Action::Seek(-10.0),
        MediaControlEvent::SeekBy(dir, d) => {
            let s = d.as_secs_f64();
            Action::Seek(if matches!(dir, SeekDirection::Forward) { s } else { -s })
        }
        _ => return None,
    })
}
