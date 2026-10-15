//! Which language each subtitle line is in. Films mix languages (Spanish
//! dialogue in an English film); each line must go to the right
//! translation model. Uses whatlang (trigram statistics, pure Rust).

use rj_core::lang::Lang;
use whatlang::{Detector, Lang as W};

/// Languages we expect in films; restricting the set makes short lines far
/// more reliable than open detection.
const CANDIDATES: &[W] = &[
    W::Eng, W::Spa, W::Fra, W::Deu, W::Ita, W::Por, W::Nld, W::Rus, W::Pol, W::Swe, W::Dan, W::Nob, W::Fin, W::Tur, W::Ara,
    W::Jpn, W::Kor, W::Cmn, W::Hin,
];

/// Shorter lines are too ambiguous to judge on their own.
const MIN_CHARS: usize = 12;
/// whatlang's confidence is low on short text even when its top guess is
/// right; below this it's mostly noise.
const MIN_CONFIDENCE: f64 = 0.15;

/// Detected language per line. Uncertain lines take the language around
/// them when both sides agree, else `fallback` (the audio track's language).
pub fn detect_lines(lines: &[String], fallback: Option<&Lang>) -> Vec<Option<Lang>> {
    let detector = Detector::with_allowlist(CANDIDATES.to_vec());
    let fallback_w = fallback.and_then(|f| CANDIDATES.iter().copied().find(|w| Lang::parse(w.code()).as_ref() == Some(f)));
    let detected: Vec<Option<Lang>> = lines
        .iter()
        .map(|l| {
            if l.chars().filter(|c| c.is_alphabetic()).count() < MIN_CHARS {
                return None;
            }
            let info = detector.detect(l)?;
            if info.confidence() < MIN_CONFIDENCE {
                return None;
            }
            let mut guess = info.lang();
            // A weak guess against the track's language: decide between just
            // those two, which is far more accurate than the open vote.
            if let Some(fw) = fallback_w.filter(|fw| *fw != guess && info.confidence() < 0.35) {
                guess = Detector::with_allowlist(vec![guess, fw]).detect_lang(l).unwrap_or(fw);
            }
            Lang::parse(guess.code())
        })
        .collect();
    (0..lines.len())
        .map(|i| {
            if let Some(l) = &detected[i] {
                return Some(l.clone());
            }
            // Inside a run of one language (same before and after, within 3
            // lines) take that; otherwise the track's language.
            let prev = (1..=3).filter_map(|d| i.checked_sub(d)).find_map(|j| detected[j].clone());
            let next = (1..=3).map(|d| i + d).filter(|&j| j < detected.len()).find_map(|j| detected[j].clone());
            match (prev, next) {
                (Some(p), Some(n)) if p == n => Some(p),
                (p, n) => fallback.cloned().or(p).or(n),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(s: &str) -> Lang {
        Lang::parse(s).unwrap()
    }

    #[test]
    fn detects_mixed_english_and_spanish() {
        let lines: Vec<String> = [
            "And so, my fellow Americans, ask not what your country can do for you.",
            "Entonces, una noche, cuando ella se acercaba el día y tendría que volver a ser flor.",
            "Y él así lo hizo.",
            "Si cuando vuelves más tarde, vienes al jardín y me arrancas, quedaré libre.",
            "Ask what you can do for your country.",
            "OK.",
            "I don't know what you mean.",
            "¿Dónde estabas anoche?",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let langs = detect_lines(&lines, Some(&l("en")));
        let codes: Vec<&str> = langs.iter().map(|l| l.as_ref().map_or("?", |l| l.as_str())).collect();
        assert_eq!(codes, ["en", "es", "es", "es", "en", "en", "en", "es"]);
    }

    #[test]
    fn short_dutch_lines_stay_dutch() {
        let lines = vec!["Waar was je gisteravond?".to_owned(), "Ik weet niet wat je bedoelt.".to_owned()];
        assert_eq!(detect_lines(&lines, Some(&l("nl"))), [Some(l("nl")), Some(l("nl"))]);
    }

    #[test]
    fn falls_back_to_track_language() {
        let lines = vec!["Hm.".to_owned(), "Ja.".to_owned()];
        assert_eq!(detect_lines(&lines, Some(&l("nl"))), [Some(l("nl")), Some(l("nl"))]);
    }
}

