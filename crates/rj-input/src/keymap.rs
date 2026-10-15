//! Key chords and the context-sensitive keymap (SPEC §4.3).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::str::FromStr;

use crate::action::{Action, ParseActionError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Context {
    Global,
    Browse,
    Dialog,
    Player,
}

impl Context {
    fn from_section(s: &str) -> Option<Self> {
        Some(match s {
            "global" => Self::Global,
            "browse" => Self::Browse,
            "dialog" => Self::Dialog,
            "player" => Self::Player,
            _ => return None,
        })
    }

    pub fn section(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Browse => "browse",
            Self::Dialog => "dialog",
            Self::Player => "player",
        }
    }

    pub const ALL: [Context; 4] = [Context::Player, Context::Browse, Context::Global, Context::Dialog];
}

/// A key plus modifiers. Keys are stored normalised: single letters upper-case,
/// named keys in their canonical spelling, gamepad buttons as `Pad.<Button>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyChord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: String,
}

impl KeyChord {
    pub fn new(key: &str, ctrl: bool, alt: bool, shift: bool) -> Self {
        let key = normalise_key(key);
        // For punctuation the shift state is already part of the character
        // ("?" is Shift+/), so it must not be required again.
        let shift = shift && !is_punctuation(&key);
        Self { ctrl, alt, shift, key }
    }
}

fn is_punctuation(key: &str) -> bool {
    let mut chars = key.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii_punctuation())
}

fn normalise_key(key: &str) -> String {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c.to_ascii_uppercase().to_string(),
        _ => match key.to_ascii_lowercase().as_str() {
            "escape" | "esc" => "Esc".into(),
            "return" | "enter" => "Enter".into(),
            " " | "space" => "Space".into(),
            "pgup" | "pageup" => "PageUp".into(),
            "pgdn" | "pagedown" => "PageDown".into(),
            "left" => "Left".into(),
            "right" => "Right".into(),
            "up" => "Up".into(),
            "down" => "Down".into(),
            "backspace" => "Backspace".into(),
            "tab" => "Tab".into(),
            _ => key.into(),
        },
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseChordError {
    #[error("empty key chord")]
    Empty,
    #[error("unknown modifier `{0}`")]
    Modifier(String),
}

impl FromStr for KeyChord {
    type Err = ParseChordError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // "Ctrl++" binds the plus key: split on '+' but keep a trailing "+".
        let (mods, key) = match s.strip_suffix("++") {
            Some(rest) => (rest, "+"),
            None => match s.rsplit_once('+') {
                Some((m, k)) if !k.is_empty() => (m, k),
                _ => ("", s),
            },
        };
        if key.trim().is_empty() {
            return Err(ParseChordError::Empty);
        }
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        for m in mods.split('+').filter(|m| !m.is_empty()) {
            match m.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" => alt = true,
                "shift" => shift = true,
                other => return Err(ParseChordError::Modifier(other.into())),
            }
        }
        Ok(Self::new(key.trim(), ctrl, alt, shift))
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        f.write_str(&self.key)
    }
}

/// A problem in a user keymap file. These are reported, never fatal.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum KeymapWarning {
    #[error("keymap is not valid TOML: {0}")]
    Toml(String),
    #[error("unknown section [{0}]")]
    Section(String),
    #[error("[{section}] `{chord}`: {error}")]
    Chord { section: String, chord: String, error: ParseChordError },
    #[error("[{section}] `{chord}`: {error}")]
    Action { section: String, chord: String, error: ParseActionError },
}

#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: HashMap<Context, HashMap<KeyChord, Action>>,
}

const DEFAULT_KEYMAP: &str = include_str!("default_keymap.toml");

impl Keymap {
    pub fn defaults() -> Self {
        let (map, warnings) = Self::default().overlay(DEFAULT_KEYMAP);
        debug_assert!(warnings.is_empty(), "default keymap invalid: {warnings:?}");
        map
    }

    /// Applies a user keymap on top of `self`; bad entries are skipped and
    /// returned as warnings so one typo doesn't lose the whole file.
    pub fn overlay(mut self, toml_text: &str) -> (Self, Vec<KeymapWarning>) {
        let mut warnings = Vec::new();
        let table: BTreeMap<String, BTreeMap<String, String>> = match toml::from_str(toml_text) {
            Ok(t) => t,
            Err(e) => return (self, vec![KeymapWarning::Toml(e.to_string())]),
        };
        for (section, entries) in table {
            let Some(ctx) = Context::from_section(&section) else {
                warnings.push(KeymapWarning::Section(section));
                continue;
            };
            let ctx_map = self.bindings.entry(ctx).or_default();
            for (chord_text, action_text) in entries {
                let chord = match chord_text.parse::<KeyChord>() {
                    Ok(c) => c,
                    Err(error) => {
                        warnings.push(KeymapWarning::Chord { section: section.clone(), chord: chord_text, error });
                        continue;
                    }
                };
                if action_text.trim() == "none" {
                    ctx_map.remove(&chord);
                    continue;
                }
                match action_text.parse::<Action>() {
                    Ok(action) => {
                        ctx_map.insert(chord, action);
                    }
                    Err(error) => {
                        warnings.push(KeymapWarning::Action { section: section.clone(), chord: chord_text, error })
                    }
                }
            }
        }
        (self, warnings)
    }

    pub fn set(&mut self, ctx: Context, chord: KeyChord, action: Action) {
        self.bindings.entry(ctx).or_default().insert(chord, action);
    }

    pub fn unbind(&mut self, ctx: Context, chord: &KeyChord) {
        if let Some(m) = self.bindings.get_mut(&ctx) {
            m.remove(chord);
        }
    }

    pub fn chords_for(&self, ctx: Context, action: Action) -> Vec<KeyChord> {
        let mut v: Vec<KeyChord> =
            self.bindings.get(&ctx).into_iter().flatten().filter(|(_, a)| **a == action).map(|(k, _)| k.clone()).collect();
        v.sort();
        v
    }

    /// Only the differences from the defaults, as keymap.toml text
    /// (`"none"` removes a default binding).
    pub fn to_user_toml(&self) -> String {
        let defaults = Self::defaults();
        let mut out = String::from("# Your key changes on top of the defaults (written by the keymap editor).\n");
        for ctx in Context::ALL {
            let mine = self.bindings.get(&ctx);
            let theirs = defaults.bindings.get(&ctx);
            let mut lines: Vec<String> = Vec::new();
            for (chord, action) in mine.into_iter().flatten() {
                if theirs.and_then(|t| t.get(chord)) != Some(action) {
                    lines.push(format!("{:?} = {:?}", chord.to_string(), action.config_name()));
                }
            }
            for chord in theirs.into_iter().flatten().map(|(c, _)| c) {
                if mine.is_none_or(|m| !m.contains_key(chord)) {
                    lines.push(format!("{:?} = \"none\"", chord.to_string()));
                }
            }
            if !lines.is_empty() {
                lines.sort();
                out.push_str(&format!("\n[{}]\n{}\n", ctx.section(), lines.join("\n")));
            }
        }
        out
    }

    /// Context bindings win over global ones.
    pub fn lookup(&self, ctx: Context, chord: &KeyChord) -> Option<Action> {
        [ctx, Context::Global]
            .iter()
            .find_map(|c| self.bindings.get(c)?.get(chord).copied())
    }

    /// Sorted bindings for one context, for the `?` cheat sheet.
    pub fn bindings(&self, ctx: Context) -> Vec<(KeyChord, Action)> {
        let mut v: Vec<_> = self
            .bindings
            .get(&ctx)
            .into_iter()
            .flatten()
            .map(|(k, a)| (k.clone(), *a))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(s: &str) -> KeyChord {
        s.parse().unwrap()
    }

    #[test]
    fn defaults_are_valid() {
        let (_, warnings) = Keymap::default().overlay(DEFAULT_KEYMAP);
        assert_eq!(warnings, []);
    }

    #[test]
    fn parses_chords() {
        assert_eq!(chord("Ctrl+Left"), KeyChord::new("Left", true, false, false));
        assert_eq!(chord("shift+j"), KeyChord::new("J", false, false, true));
        assert_eq!(chord("Ctrl++").key, "+");
        assert_eq!(chord("escape").key, "Esc");
        assert!("Hyper+X".parse::<KeyChord>().is_err());
    }

    #[test]
    fn punctuation_ignores_shift() {
        // Slint reports "?" with shift held.
        assert_eq!(KeyChord::new("?", false, false, true), chord("?"));
    }

    #[test]
    fn context_overrides_global() {
        let km = Keymap::defaults();
        assert_eq!(km.lookup(Context::Player, &chord("Left")), Some(Action::Seek(-5.0)));
        assert_eq!(km.lookup(Context::Browse, &chord("Left")), Some(Action::NavLeft));
        assert_eq!(km.lookup(Context::Player, &chord("F")), Some(Action::ToggleFullscreen));
    }

    #[test]
    fn edits_round_trip_through_user_toml() {
        let mut km = Keymap::defaults();
        km.set(Context::Player, chord("Ctrl+K"), Action::PlayPause);
        km.unbind(Context::Player, &chord("K"));
        km.set(Context::Player, chord("Left"), Action::Seek(-10.0));
        let text = km.to_user_toml();
        let (back, warnings) = Keymap::defaults().overlay(&text);
        assert_eq!(warnings, [], "{text}");
        assert_eq!(back.lookup(Context::Player, &chord("Ctrl+K")), Some(Action::PlayPause));
        assert_eq!(back.lookup(Context::Player, &chord("K")), None);
        assert_eq!(back.lookup(Context::Player, &chord("Left")), Some(Action::Seek(-10.0)));
        // Untouched defaults aren't written out.
        assert!(!text.contains("\"Space\""), "{text}");
        assert_eq!(Keymap::defaults().to_user_toml().lines().count(), 1);
    }

    #[test]
    fn user_overlay_replaces_and_reports_bad_entries() {
        let user = "[player]\n\"Left\" = \"seek:-10\"\n\"Q\" = \"warp\"\n[bogus]\n";
        let (km, warnings) = Keymap::defaults().overlay(user);
        assert_eq!(km.lookup(Context::Player, &chord("Left")), Some(Action::Seek(-10.0)));
        assert_eq!(warnings.len(), 2);
        // Untouched defaults survive.
        assert_eq!(km.lookup(Context::Player, &chord("Space")), Some(Action::PlayPause));
    }
}
