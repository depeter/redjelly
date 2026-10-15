//! Input handling. Every source — keyboard, gamepad, media keys — is turned
//! into an [`Action`] via the [`Keymap`]; the UI and player never see raw keys.

pub mod action;
#[cfg(feature = "gamepad")]
pub mod gamepad;
pub mod keymap;
#[cfg(feature = "media-controls")]
pub mod media_controls;

pub use action::Action;
pub use keymap::{Context, KeyChord, Keymap};
