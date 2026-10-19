//! The player's on-screen controls: labelled buttons, seek bar and volume.
//! Mouse and touch use them directly; with keys or a controller, Enter / A
//! shows the bar with Play focused and the arrows move between buttons.

use std::time::Instant;

use rj_core::lang::Lang;
use rj_input::Action;
use rj_player::MpvTrack;
use slint::ComponentHandle;

use crate::subtitles::{Panel, PanelAction, PanelKind, entry};
use crate::{App, PanelItem};

// Focus ids, shared with `Controls` in ui/app.slint.
pub const STOP: i32 = 0;
pub const PREVIOUS: i32 = 1;
pub const REWIND: i32 = 2;
pub const PLAY: i32 = 3;
pub const FORWARD: i32 = 4;
pub const NEXT: i32 = 5;
pub const SUBTITLES: i32 = 6;
pub const AUDIO: i32 = 7;
pub const VOLUME: i32 = 8;
pub const FULLSCREEN: i32 = 9;
pub const SEEK_BAR: i32 = 10;

/// The button row, left to right.
const ROW: [i32; 10] = [STOP, PREVIOUS, REWIND, PLAY, FORWARD, NEXT, SUBTITLES, AUDIO, VOLUME, FULLSCREEN];

const REWIND_SECS: f64 = -10.0;
const FORWARD_SECS: f64 = 30.0;

/// "English · SDH" plus details ("Director's commentary · 5.1 · built in").
pub fn track_label(t: &MpvTrack) -> (String, String) {
    let mut title = t.lang.as_deref().and_then(Lang::parse).map_or_else(|| "Unknown language".to_owned(), |l| l.name());
    if t.forced {
        title.push_str(" · forced");
    }
    if t.hearing_impaired {
        title.push_str(" · SDH");
    }
    let mut detail: Vec<String> = t.title.iter().cloned().collect();
    if t.kind == "audio" {
        match t.channels {
            0 => {}
            1 => detail.push("mono".into()),
            2 => detail.push("stereo".into()),
            6 => detail.push("5.1".into()),
            8 => detail.push("7.1".into()),
            n => detail.push(format!("{n} channels")),
        }
    } else {
        detail.push(if t.external { "downloaded" } else { "built in" }.into());
    }
    (title, detail.join(" · "))
}

impl App {
    pub fn controls_engaged(&self) -> bool {
        self.controls_focus.get().is_some()
    }

    fn focus_control(&self, focus: Option<i32>) {
        self.controls_focus.set(focus);
        self.ui().set_player_focus(focus.unwrap_or(-1));
    }

    pub fn release_controls(&self) {
        self.focus_control(None);
    }

    /// Hides the bar now (Esc) instead of waiting for it to time out.
    pub fn hide_controls(&self) {
        self.release_controls();
        self.osd_until.set(Instant::now());
        self.ui().set_player_osd(false);
    }

    fn control_shown(&self, button: i32) -> bool {
        let ui = self.ui();
        match button {
            PREVIOUS => ui.get_player_has_previous(),
            NEXT => ui.get_player_has_next(),
            _ => true,
        }
    }

    /// Enter / A while watching: skip the intro if offered, otherwise bring
    /// up the bar; once it's up, press the focused button.
    pub fn controls_activate(&self) {
        match self.controls_focus.get() {
            Some(SEEK_BAR) => self.handle_player(Action::PlayPause),
            Some(button) => self.press_control(button),
            None if self.skip_segment_or_play_next() => {}
            None => {
                self.focus_control(Some(PLAY));
                self.show_osd();
            }
        }
    }

    /// Arrow keys / D-pad while the controls have focus.
    pub fn controls_nav(&self, action: Action) {
        let Some(focus) = self.controls_focus.get() else { return };
        self.show_osd();
        match (action, focus) {
            (Action::NavLeft, SEEK_BAR) => self.handle_player(Action::Seek(REWIND_SECS)),
            (Action::NavRight, SEEK_BAR) => self.handle_player(Action::Seek(-REWIND_SECS)),
            (Action::NavDown, SEEK_BAR) => self.focus_control(Some(PLAY)),
            (Action::NavUp, _) => self.focus_control(Some(SEEK_BAR)),
            (Action::NavLeft | Action::NavRight, button) => {
                let pos = ROW.iter().position(|b| *b == button).unwrap_or(0);
                let target = if action == Action::NavLeft {
                    ROW[..pos].iter().rev().find(|b| self.control_shown(**b))
                } else {
                    ROW[pos + 1..].iter().find(|b| self.control_shown(**b))
                };
                if let Some(b) = target {
                    self.focus_control(Some(*b));
                }
            }
            (Action::Activate, _) => self.controls_activate(),
            (Action::Back, _) => self.hide_controls(),
            _ => {}
        }
    }

    /// A button was clicked, or pressed with Enter while focused.
    pub fn press_control(&self, button: i32) {
        match button {
            STOP => self.handle_player(Action::Stop),
            PREVIOUS => self.handle_player(Action::PreviousEpisode),
            REWIND => self.handle_player(Action::Seek(REWIND_SECS)),
            PLAY => self.handle_player(Action::PlayPause),
            FORWARD => self.handle_player(Action::Seek(FORWARD_SECS)),
            NEXT => self.handle_player(Action::NextEpisode),
            SUBTITLES => {
                self.release_controls();
                self.handle_player(Action::SubtitlePanel);
            }
            AUDIO => {
                self.release_controls();
                self.open_audio_panel();
            }
            VOLUME => self.handle_player(Action::Mute),
            FULLSCREEN => {
                self.show_osd();
                let ui = self.ui();
                ui.window().set_fullscreen(!ui.window().is_fullscreen());
            }
            _ => {}
        }
    }

    /// A click or tap on the video: closes a panel, then shows the bar,
    /// then pauses (so a touchscreen tap never pauses by surprise).
    pub fn video_clicked(&self) {
        let ui = self.ui();
        if self.panel_open() {
            self.close_panel();
        } else if !ui.get_player_osd() && !ui.get_player_paused() {
            self.show_osd();
        } else {
            self.handle_player(Action::PlayPause);
        }
    }

    pub fn seek_to_fraction(&self, fraction: f32) {
        self.show_osd();
        let pct = (f64::from(fraction) * 100.0).clamp(0.0, 100.0);
        let _ = self.player.mpv().command("seek", &[&pct.to_string(), "absolute-percent"]);
    }

    pub fn set_volume_fraction(&self, fraction: f32) {
        self.show_osd();
        let mpv = self.player.mpv();
        let _ = mpv.set_property("mute", false);
        let _ = mpv.set_property("volume", (f64::from(fraction) * 100.0).round());
    }

    /// Mirrors volume into the bar; called every tick.
    pub fn update_controls(&self) {
        let mpv = self.player.mpv();
        let ui = self.ui();
        ui.set_player_volume((mpv.get_property::<f64>("volume").unwrap_or(100.0) / 100.0) as f32);
        ui.set_player_muted(mpv.get_property::<bool>("mute").unwrap_or(false));
    }

    pub fn open_audio_panel(&self) {
        *self.panel.borrow_mut() = Some(Panel { kind: PanelKind::Audio, ..Default::default() });
        let ui = self.ui();
        ui.set_panel_title("Audio".into());
        ui.set_panel_status("Choose the language you want to hear".into());
        ui.set_panel_visible(true);
        self.refresh_panel();
        let current = self.player.mpv().get_property::<i64>("aid").ok();
        self.focus_panel_entry(|a| current.is_some_and(|id| *a == PanelAction::Audio(id)));
    }

    pub fn audio_panel_entries(&self) -> Vec<(PanelItem, PanelAction)> {
        let current = self.player.mpv().get_property::<i64>("aid").ok();
        let mut entries: Vec<_> = self
            .player
            .tracks()
            .iter()
            .filter(|t| t.kind == "audio")
            .map(|t| {
                let (title, detail) = track_label(t);
                let mut e = entry(title, detail, PanelAction::Audio(t.id));
                if Some(t.id) == current {
                    e.0.badge = "PLAYING".into();
                }
                e
            })
            .collect();
        if entries.is_empty() {
            entries.push(entry("This video has no audio tracks", "", PanelAction::None));
        }
        entries
    }

    /// "Off" plus every subtitle track of the video, for the subtitle panel.
    pub fn subtitle_track_entries(&self) -> Vec<(PanelItem, PanelAction)> {
        let mpv = self.player.mpv();
        let current = mpv.get_property::<i64>("sid").ok();
        let delay = (mpv.get_property::<f64>("sub-delay").unwrap_or(0.0) * 1000.0).round() as i64;
        let mut off = entry("Off", "No subtitles", PanelAction::Subtitle(None));
        if current.is_none() {
            off.0.badge = "ON".into();
        }
        let mut entries = vec![off];
        for t in self.player.tracks().iter().filter(|t| t.kind == "sub") {
            let (title, mut detail) = track_label(t);
            let on = Some(t.id) == current;
            if on && delay != 0 {
                detail.push_str(&format!(" · delay {:+.1} s", delay as f64 / 1000.0));
            }
            let mut e = entry(title, detail, PanelAction::Subtitle(Some(t.id)));
            if on {
                e.0.badge = "ON".into();
            }
            entries.push(e);
        }
        entries
    }

    pub fn choose_audio(&self, id: i64) {
        let _ = self.player.set_audio(id);
        self.close_panel();
        let msg = self.remember_audio_choice();
        self.toast(msg);
    }

    pub fn choose_subtitle(&self, id: Option<i64>) {
        let _ = self.player.set_subtitle(id);
        let _ = self.player.mpv().set_property("sub-visibility", true);
        self.close_panel();
        let msg = self.remember_subtitle_choice();
        self.toast(msg);
    }
}
