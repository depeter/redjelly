//! Gamepad input via gilrs (XInput on Windows, evdev on Linux).
//!
//! Buttons become `Pad.<Name>` chords so they share the keymap with the
//! keyboard. The left stick acts as a D-pad (edge-triggered).

use gilrs::{Axis, Button, EventType, Gilrs};

use crate::keymap::KeyChord;

const STICK_THRESHOLD: f32 = 0.6;

pub struct Gamepads {
    gilrs: Gilrs,
    stick_x: i8,
    stick_y: i8,
}

impl Gamepads {
    pub fn new() -> Result<Self, Box<gilrs::Error>> {
        Ok(Self { gilrs: Gilrs::new().map_err(Box::new)?, stick_x: 0, stick_y: 0 })
    }

    /// Drains pending events; call from a ~60 Hz timer on the UI thread.
    pub fn poll(&mut self) -> Vec<KeyChord> {
        let mut out = Vec::new();
        while let Some(ev) = self.gilrs.next_event() {
            match ev.event {
                EventType::ButtonPressed(button, _) => {
                    if let Some(name) = button_name(button) {
                        out.push(pad(name));
                    }
                }
                EventType::AxisChanged(Axis::LeftStickX, v, _) => {
                    if let Some(name) = edge(&mut self.stick_x, v, "DPadLeft", "DPadRight") {
                        out.push(pad(name));
                    }
                }
                EventType::AxisChanged(Axis::LeftStickY, v, _) => {
                    // gilrs: positive Y is up.
                    if let Some(name) = edge(&mut self.stick_y, v, "DPadDown", "DPadUp") {
                        out.push(pad(name));
                    }
                }
                _ => {}
            }
        }
        out
    }
}

fn pad(name: &str) -> KeyChord {
    KeyChord::new(&format!("Pad.{name}"), false, false, false)
}

/// Emits once when the stick crosses the threshold, then waits for it to
/// return to centre.
fn edge(state: &mut i8, v: f32, neg: &'static str, pos: &'static str) -> Option<&'static str> {
    let now = if v > STICK_THRESHOLD { 1 } else if v < -STICK_THRESHOLD { -1 } else { 0 };
    let fired = (now != *state && now != 0).then_some(if now > 0 { pos } else { neg });
    *state = now;
    fired
}

fn button_name(b: Button) -> Option<&'static str> {
    Some(match b {
        Button::South => "South",
        Button::East => "East",
        Button::North => "North",
        Button::West => "West",
        // gilrs calls the bumpers "triggers" and the analog triggers "trigger2".
        Button::LeftTrigger => "LeftShoulder",
        Button::RightTrigger => "RightShoulder",
        Button::LeftTrigger2 => "LeftTrigger",
        Button::RightTrigger2 => "RightTrigger",
        Button::Select => "Back",
        Button::Start => "Start",
        Button::Mode => "Guide",
        Button::LeftThumb => "LeftStick",
        Button::RightThumb => "RightStick",
        Button::DPadUp => "DPadUp",
        Button::DPadDown => "DPadDown",
        Button::DPadLeft => "DPadLeft",
        Button::DPadRight => "DPadRight",
        _ => return None,
    })
}
