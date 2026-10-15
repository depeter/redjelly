//! Automatic audio/subtitle track selection (SPEC §5.1).
//!
//! Rule set: audio follows the language preference list; subtitles are always
//! on and also follow the preference list. Per-series memory overrides both.

use crate::lang::Lang;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioTrack {
    /// Jellyfin `MediaStream.Index`.
    pub index: u32,
    pub lang: Option<Lang>,
    pub channels: u32,
    pub is_default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubKind {
    /// SRT, ASS, VTT — restylable.
    Text,
    /// PGS, VobSub — bitmap.
    Image,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleTrack {
    pub index: u32,
    pub lang: Option<Lang>,
    pub kind: SubKind,
    pub forced: bool,
    pub hearing_impaired: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Preferences {
    /// Ordered, most preferred first (e.g. nl, en).
    pub languages: Vec<Lang>,
    pub prefer_sdh: bool,
}

/// Languages remembered for the series/movie after a manual switch (SPEC §5.2).
#[derive(Debug, Clone, Default)]
pub struct Remembered {
    pub audio: Option<Lang>,
    /// `Some(None)` means "user turned subtitles off for this series".
    pub subtitle: Option<Option<Lang>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub audio: Option<u32>,
    pub subtitle: Option<u32>,
    /// The top preference language has no subtitle: start a provider search.
    pub search_for: Option<Lang>,
}

pub fn select(
    audio: &[AudioTrack],
    subs: &[SubtitleTrack],
    prefs: &Preferences,
    remembered: &Remembered,
) -> Selection {
    let top = prefs.languages.first();
    let has_top_sub = top.is_some_and(|t| subs.iter().any(|s| s.lang.as_ref() == Some(t) && !s.forced));

    Selection {
        audio: select_audio(audio, prefs, remembered),
        subtitle: select_subtitle(subs, prefs, remembered),
        search_for: if has_top_sub { None } else { top.cloned() },
    }
}

fn select_audio(tracks: &[AudioTrack], prefs: &Preferences, remembered: &Remembered) -> Option<u32> {
    let best_in = |lang: &Lang| {
        tracks
            .iter()
            .filter(|t| t.lang.as_ref() == Some(lang))
            .max_by_key(|t| (t.is_default, t.channels, std::cmp::Reverse(t.index)))
    };

    remembered
        .audio
        .iter()
        .chain(&prefs.languages)
        .find_map(best_in)
        .or_else(|| tracks.iter().find(|t| t.is_default))
        .or_else(|| tracks.first())
        .map(|t| t.index)
}

fn select_subtitle(tracks: &[SubtitleTrack], prefs: &Preferences, remembered: &Remembered) -> Option<u32> {
    let best_in = |lang: &Lang| {
        tracks
            .iter()
            .filter(|t| t.lang.as_ref() == Some(lang))
            .max_by_key(|t| {
                (
                    !t.forced,
                    t.hearing_impaired == prefs.prefer_sdh,
                    t.kind == SubKind::Text,
                    t.is_default,
                    std::cmp::Reverse(t.index),
                )
            })
    };

    match &remembered.subtitle {
        Some(None) => return None,
        Some(Some(lang)) => {
            if let Some(t) = best_in(lang) {
                return Some(t.index);
            }
        }
        None => {}
    }
    prefs.languages.iter().find_map(best_in).map(|t| t.index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(s: &str) -> Option<Lang> {
        Lang::parse(s)
    }
    fn audio(index: u32, lang: &str, channels: u32, is_default: bool) -> AudioTrack {
        AudioTrack { index, lang: l(lang), channels, is_default }
    }
    fn sub(index: u32, lang: &str) -> SubtitleTrack {
        SubtitleTrack {
            index,
            lang: l(lang),
            kind: SubKind::Text,
            forced: false,
            hearing_impaired: false,
            is_default: false,
        }
    }
    fn nl_en() -> Preferences {
        Preferences { languages: vec![l("nl").unwrap(), l("en").unwrap()], prefer_sdh: false }
    }

    #[test]
    fn english_film_gets_english_audio_and_dutch_subs() {
        let a = [audio(1, "eng", 6, true), audio(2, "fre", 2, false)];
        let s = [sub(3, "eng"), sub(4, "dut")];
        let sel = select(&a, &s, &nl_en(), &Remembered::default());
        assert_eq!(sel, Selection { audio: Some(1), subtitle: Some(4), search_for: None });
    }

    #[test]
    fn dutch_dub_preferred_when_present() {
        let a = [audio(1, "eng", 6, true), audio(2, "dut", 2, false)];
        assert_eq!(select(&a, &[], &nl_en(), &Remembered::default()).audio, Some(2));
    }

    #[test]
    fn falls_back_to_english_sub_and_searches_dutch() {
        let s = [sub(3, "eng"), sub(4, "fre")];
        let sel = select(&[], &s, &nl_en(), &Remembered::default());
        assert_eq!(sel.subtitle, Some(3));
        assert_eq!(sel.search_for, l("nl"));
    }

    #[test]
    fn full_sub_beats_forced_and_sdh() {
        let mut forced = sub(3, "dut");
        forced.forced = true;
        let mut sdh = sub(4, "dut");
        sdh.hearing_impaired = true;
        let s = [forced, sdh, sub(5, "dut")];
        assert_eq!(select(&[], &s, &nl_en(), &Remembered::default()).subtitle, Some(5));
    }

    #[test]
    fn forced_only_still_triggers_search() {
        let mut forced = sub(3, "dut");
        forced.forced = true;
        let sel = select(&[], &[forced], &nl_en(), &Remembered::default());
        assert_eq!(sel.subtitle, Some(3));
        assert_eq!(sel.search_for, l("nl"));
    }

    #[test]
    fn text_beats_image() {
        let mut pgs = sub(3, "dut");
        pgs.kind = SubKind::Image;
        let s = [pgs, sub(4, "dut")];
        assert_eq!(select(&[], &s, &nl_en(), &Remembered::default()).subtitle, Some(4));
    }

    #[test]
    fn remembered_languages_win() {
        let a = [audio(1, "eng", 6, true), audio(2, "jpn", 2, false)];
        let s = [sub(3, "eng"), sub(4, "dut")];
        let r = Remembered { audio: l("ja"), subtitle: Some(l("en")) };
        let sel = select(&a, &s, &nl_en(), &r);
        assert_eq!((sel.audio, sel.subtitle), (Some(2), Some(3)));
    }

    #[test]
    fn remembered_off_disables_subs() {
        let r = Remembered { audio: None, subtitle: Some(None) };
        assert_eq!(select(&[], &[sub(4, "dut")], &nl_en(), &r).subtitle, None);
    }

    #[test]
    fn unknown_languages_fall_back_to_default_audio() {
        let a = [audio(1, "und", 2, false), audio(2, "ita", 2, true)];
        assert_eq!(select(&a, &[], &nl_en(), &Remembered::default()).audio, Some(2));
    }
}
