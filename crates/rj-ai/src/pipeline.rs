//! Stage traits and the translation routing (SPEC §7 steps 2–4).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rj_core::lang::Lang;

use crate::{Cue, Error, srt};

/// Speech-to-text over a 16 kHz mono WAV file.
pub trait Transcriber {
    /// `translate_to_english` uses Whisper's built-in X→en translation.
    fn transcribe(
        &mut self,
        audio: &Path,
        spoken: Option<&Lang>,
        translate_to_english: bool,
        progress: &mut dyn FnMut(f32),
        cancel: &AtomicBool,
    ) -> Result<Vec<Cue>, Error>;
}

/// Text translation between language pairs (Opus-MT models).
pub trait Translator {
    fn supports(&self, from: &Lang, to: &Lang) -> bool;
    fn translate(
        &mut self,
        lines: &[String],
        from: &Lang,
        to: &Lang,
        progress: &mut dyn FnMut(f32),
        cancel: &AtomicBool,
    ) -> Result<Vec<String>, Error>;
}

/// Translates an existing subtitle's lines, keeping its timing (the cheap
/// "translate the English subtitle" path, SPEC §7).
pub fn translate_cues(
    cues: Vec<Cue>,
    from: &Lang,
    to: &Lang,
    translator: &mut dyn Translator,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<Vec<Cue>, Error> {
    let lines: Vec<String> = cues.iter().map(|c| c.text.replace('\n', " ").trim().to_owned()).collect();
    let translated = translator.translate(&lines, from, to, progress, cancel)?;
    if translated.len() != cues.len() {
        return Err(Error::Translate("line count mismatch".into()));
    }
    let cues = cues.into_iter().zip(translated).map(|(c, text)| Cue { text, ..c }).collect();
    Ok(srt::clean(cues))
}

/// Translator for when no model is installed: supports nothing.
pub struct NoTranslator;

impl Translator for NoTranslator {
    fn supports(&self, _: &Lang, _: &Lang) -> bool {
        false
    }
    fn translate(&mut self, _: &[String], _: &Lang, _: &Lang, _: &mut dyn FnMut(f32), _: &AtomicBool) -> Result<Vec<String>, Error> {
        Err(Error::Translate("no translation model installed".into()))
    }
}

/// How to get from spoken language to the target language.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// Transcribe only.
    Direct,
    /// Transcribe, then translate spoken→target.
    Translate,
    /// Whisper translates to English; done.
    WhisperToEnglish,
    /// Whisper translates to English, then en→target.
    PivotViaEnglish,
}

pub fn route(spoken: Option<&Lang>, target: &Lang, translator: &dyn Translator) -> Result<Route, Error> {
    let en = Lang::parse("en").expect("en is valid");
    match spoken {
        Some(s) if s == target => Ok(Route::Direct),
        Some(s) if translator.supports(s, target) => Ok(Route::Translate),
        _ if *target == en => Ok(Route::WhisperToEnglish),
        _ if translator.supports(&en, target) => Ok(Route::PivotViaEnglish),
        _ => Err(Error::Translate(format!("no translation model to {target}"))),
    }
}

/// Runs transcription (+ translation) and returns cleaned cues.
pub fn generate(
    audio: &Path,
    spoken: Option<&Lang>,
    target: &Lang,
    transcriber: &mut dyn Transcriber,
    translator: &mut dyn Translator,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<Vec<Cue>, Error> {
    let route = route(spoken, target, translator)?;
    let to_en = matches!(route, Route::WhisperToEnglish | Route::PivotViaEnglish);
    // Transcription dominates runtime: 90% of the progress bar.
    let mut cues = transcriber.transcribe(audio, spoken, to_en, &mut |p| progress(p * 0.8), cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }

    let from = match route {
        Route::Translate => spoken.cloned(),
        Route::PivotViaEnglish => Lang::parse("en"),
        Route::Direct | Route::WhisperToEnglish => None,
    };
    if let Some(from) = from {
        // Two-line cues translate better as one sentence; srt::clean re-wraps.
        let lines: Vec<String> = cues.iter().map(|c| c.text.replace('\n', " ").trim().to_owned()).collect();
        let translated = translator.translate(&lines, &from, target, &mut |p| progress(0.8 + p * 0.2), cancel)?;
        if translated.len() != cues.len() {
            return Err(Error::Translate("line count mismatch".into()));
        }
        for (c, t) in cues.iter_mut().zip(translated) {
            c.text = t;
        }
    }
    progress(1.0);
    Ok(srt::clean(cues))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(s: &str) -> Lang {
        Lang::parse(s).unwrap()
    }

    struct FakeWhisper;
    impl Transcriber for FakeWhisper {
        fn transcribe(
            &mut self,
            _: &Path,
            _: Option<&Lang>,
            to_en: bool,
            progress: &mut dyn FnMut(f32),
            _: &AtomicBool,
        ) -> Result<Vec<Cue>, Error> {
            progress(1.0);
            let text = if to_en { "hello" } else { "konnichiwa" };
            Ok(vec![Cue { start_ms: 0, end_ms: 2000, text: text.into() }])
        }
    }

    /// Only knows en→nl, like the bundled Opus-MT model.
    struct FakeOpusMt;
    impl Translator for FakeOpusMt {
        fn supports(&self, from: &Lang, to: &Lang) -> bool {
            from.as_str() == "en" && to.as_str() == "nl"
        }
        fn translate(
            &mut self,
            lines: &[String],
            _: &Lang,
            _: &Lang,
            _: &mut dyn FnMut(f32),
            _: &AtomicBool,
        ) -> Result<Vec<String>, Error> {
            Ok(lines.iter().map(|l| if l == "hello" { "hallo".into() } else { l.clone() }).collect())
        }
    }

    #[test]
    fn routes() {
        let t = FakeOpusMt;
        assert_eq!(route(Some(&l("nl")), &l("nl"), &t).unwrap(), Route::Direct);
        assert_eq!(route(Some(&l("en")), &l("nl"), &t).unwrap(), Route::Translate);
        assert_eq!(route(Some(&l("ja")), &l("en"), &t).unwrap(), Route::WhisperToEnglish);
        assert_eq!(route(Some(&l("ja")), &l("nl"), &t).unwrap(), Route::PivotViaEnglish);
        assert_eq!(route(None, &l("nl"), &t).unwrap(), Route::PivotViaEnglish);
        assert!(route(Some(&l("ja")), &l("de"), &t).is_err());
    }

    #[test]
    fn japanese_to_dutch_pivots() {
        let cancel = AtomicBool::new(false);
        let mut last = 0.0;
        let cues =
            generate(Path::new("x.wav"), Some(&l("ja")), &l("nl"), &mut FakeWhisper, &mut FakeOpusMt, &mut |p| last = p, &cancel)
                .unwrap();
        assert_eq!(cues[0].text, "hallo");
        assert_eq!(last, 1.0);
    }
}
