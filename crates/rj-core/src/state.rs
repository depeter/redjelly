//! Small persistent app state (`state.json` in the data dir): per-series
//! track memory (SPEC §5.2) and per-subtitle delays (SPEC §6.4). Kept as
//! JSON — it's tiny and human-inspectable.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::lang::Lang;
use crate::tracks::Remembered;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrackChoice {
    /// Audio language, ISO 639-1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<String>,
    /// `Some("")` = subtitles off; `Some(lang)`; `None` = not remembered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    /// Keyed by series id (episodes) or item id (movies).
    pub tracks: HashMap<String, TrackChoice>,
    /// item id → subtitle key → delay in ms.
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub sub_delays: HashMap<String, HashMap<String, i64>>,
    /// Shader preset per series / movie (e.g. Anime4K for anime shows).
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub shaders: HashMap<String, String>,
}

impl AppState {
    /// Missing or corrupt files give an empty state: losing track memory is
    /// better than refusing to start.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).expect("state serialises"))?;
        std::fs::rename(tmp, path)
    }

    pub fn remembered(&self, key: &str) -> Remembered {
        let Some(c) = self.tracks.get(key) else { return Remembered::default() };
        Remembered {
            audio: c.audio.as_deref().and_then(Lang::parse),
            subtitle: c.subtitle.as_deref().map(Lang::parse),
        }
    }

    pub fn sub_delay(&self, item: &str, sub: &str) -> i64 {
        self.sub_delays.get(item).and_then(|m| m.get(sub)).copied().unwrap_or(0)
    }

    pub fn set_sub_delay(&mut self, item: &str, sub: &str, ms: i64) {
        if ms == 0 {
            if let Some(m) = self.sub_delays.get_mut(item) {
                m.remove(sub);
                if m.is_empty() {
                    self.sub_delays.remove(item);
                }
            }
        } else {
            self.sub_delays.entry(item.into()).or_default().insert(sub.into(), ms);
        }
    }

    pub fn remember_audio(&mut self, key: &str, lang: Option<&Lang>) {
        if let Some(lang) = lang {
            self.tracks.entry(key.into()).or_default().audio = Some(lang.to_string());
        }
    }

    /// `None` records "subtitles off".
    pub fn remember_subtitle(&mut self, key: &str, lang: Option<&Lang>) {
        self.tracks.entry(key.into()).or_default().subtitle = Some(lang.map(Lang::to_string).unwrap_or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_and_round_trips() {
        let dir = std::env::temp_dir().join(format!("rj-state-{}", std::process::id()));
        let path = dir.join("state.json");
        let mut s = AppState::default();
        s.remember_audio("series1", Lang::parse("ja").as_ref());
        s.remember_subtitle("series1", None);
        s.remember_subtitle("movie1", Lang::parse("en").as_ref());
        s.save(&path).unwrap();

        let back = AppState::load(&path);
        let r = back.remembered("series1");
        assert_eq!(r.audio, Lang::parse("ja"));
        assert_eq!(r.subtitle, Some(None));
        assert_eq!(back.remembered("movie1").subtitle, Some(Lang::parse("en")));
        assert_eq!(back.remembered("unknown").audio, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sub_delays_set_and_clear() {
        let mut s = AppState::default();
        s.set_sub_delay("ep", "ext:film.nl.srt", 300);
        assert_eq!(s.sub_delay("ep", "ext:film.nl.srt"), 300);
        assert_eq!(s.sub_delay("ep", "emb:3"), 0);
        s.set_sub_delay("ep", "ext:film.nl.srt", 0);
        assert!(s.sub_delays.is_empty());
    }

    #[test]
    fn corrupt_file_is_empty_state() {
        let path = std::env::temp_dir().join(format!("rj-bad-{}.json", std::process::id()));
        std::fs::write(&path, "{nope").unwrap();
        assert!(AppState::load(&path).tracks.is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
