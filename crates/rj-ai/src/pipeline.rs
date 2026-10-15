//! Stage traits and per-line translation routing (SPEC §7 steps 2–4).
//!
//! Whisper always transcribes what is actually said (its built-in
//! translate mode is unreliable, and broken in large-v3-turbo). Each line is
//! then language-detected and translated with the right model, so films that
//! mix languages (Spanish dialogue in an English film) come out right.

use std::collections::BTreeMap;
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

/// How one source language reaches the target.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// Already in the target language.
    Keep,
    Direct,
    /// Translate to English first, then English → target.
    ViaEnglish,
    /// No model can do it; the line stays as transcribed.
    Unsupported,
}

pub fn route(from: &Lang, to: &Lang, supports: &dyn Fn(&Lang, &Lang) -> bool) -> Route {
    let en = Lang::parse("en").expect("valid");
    if from == to {
        Route::Keep
    } else if supports(from, to) {
        Route::Direct
    } else if *from != en && supports(from, &en) && (*to == en || supports(&en, to)) {
        Route::ViaEnglish
    } else {
        Route::Unsupported
    }
}

/// Result of [`translate_mixed`].
pub struct Translated {
    pub cues: Vec<Cue>,
    /// Languages that had no model; those lines kept their original text.
    pub untranslated: Vec<(Lang, usize)>,
}

/// Translates every line from its own language (`langs[i]`, from
/// [`crate::langid::detect_lines`]) into `target`, keeping timing.
pub fn translate_mixed(
    mut cues: Vec<Cue>,
    langs: &[Option<Lang>],
    default: &Lang,
    target: &Lang,
    translator: &mut dyn Translator,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<Translated, Error> {
    let en = Lang::parse("en").expect("valid");
    // Two-line cues translate better as one sentence; srt::clean re-wraps.
    let texts: Vec<String> = cues.iter().map(|c| c.text.replace('\n', " ").trim().to_owned()).collect();
    let mut groups: BTreeMap<Lang, Vec<usize>> = BTreeMap::new();
    for (i, lang) in langs.iter().enumerate().take(cues.len()) {
        groups.entry(lang.clone().unwrap_or_else(|| default.clone())).or_default().push(i);
    }
    let total = cues.len().max(1) as f32;
    let mut done = 0usize;
    let mut untranslated = Vec::new();
    for (lang, idxs) in groups {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let lines: Vec<String> = idxs.iter().map(|&i| texts[i].clone()).collect();
        let base = done as f32 / total;
        let share = idxs.len() as f32 / total;
        let out = match route(&lang, target, &|f, t| translator.supports(f, t)) {
            Route::Keep => None,
            Route::Direct => Some(translator.translate(&lines, &lang, target, &mut |p| progress(base + p * share), cancel)?),
            Route::ViaEnglish => {
                let english =
                    translator.translate(&lines, &lang, &en, &mut |p| progress(base + p * share / 2.0), cancel)?;
                if *target == en {
                    Some(english)
                } else {
                    Some(translator.translate(&english, &en, target, &mut |p| progress(base + share / 2.0 + p * share / 2.0), cancel)?)
                }
            }
            Route::Unsupported => {
                untranslated.push((lang.clone(), idxs.len()));
                None
            }
        };
        if let Some(out) = out {
            if out.len() != idxs.len() {
                return Err(Error::Translate("line count mismatch".into()));
            }
            for (&i, text) in idxs.iter().zip(out) {
                cues[i].text = text;
            }
        }
        done += idxs.len();
        progress(done as f32 / total);
    }
    Ok(Translated { cues: srt::clean(cues), untranslated })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(s: &str) -> Lang {
        Lang::parse(s).unwrap()
    }

    fn cue(text: &str) -> Cue {
        Cue { start_ms: 0, end_ms: 2000, text: text.into() }
    }

    /// Knows en→nl, es→nl and "anything"→en (like mul-en).
    struct Fake;
    impl Translator for Fake {
        fn supports(&self, from: &Lang, to: &Lang) -> bool {
            matches!((from.as_str(), to.as_str()), ("en", "nl") | ("es", "nl") | (_, "en"))
        }
        fn translate(
            &mut self,
            lines: &[String],
            from: &Lang,
            to: &Lang,
            _: &mut dyn FnMut(f32),
            _: &AtomicBool,
        ) -> Result<Vec<String>, Error> {
            Ok(lines.iter().map(|s| format!("[{from}>{to}] {s}")).collect())
        }
    }

    #[test]
    fn routes() {
        let s = |f: &Lang, t: &Lang| Fake.supports(f, t);
        assert_eq!(route(&l("nl"), &l("nl"), &s), Route::Keep);
        assert_eq!(route(&l("es"), &l("nl"), &s), Route::Direct);
        assert_eq!(route(&l("fr"), &l("nl"), &s), Route::ViaEnglish);
        assert_eq!(route(&l("fr"), &l("de"), &s), Route::Unsupported);
    }

    #[test]
    fn each_line_uses_its_own_language() {
        let cues = vec![cue("Hello"), cue("Hola"), cue("Bonjour"), cue("Hallo")];
        let langs = [Some(l("en")), Some(l("es")), Some(l("fr")), Some(l("nl"))];
        let out = translate_mixed(cues, &langs, &l("en"), &l("nl"), &mut Fake, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        let texts: Vec<_> = out.cues.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(texts, ["[en>nl] Hello", "[es>nl] Hola", "[en>nl] [fr>en] Bonjour", "Hallo"]);
        assert!(out.untranslated.is_empty());
    }

    #[test]
    fn reports_languages_without_a_model() {
        let out = translate_mixed(
            vec![cue("Hola"), cue("Ciao")],
            &[Some(l("es")), Some(l("it"))],
            &l("en"),
            &l("de"),
            &mut Fake,
            &mut |_| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(out.untranslated, [(l("es"), 1), (l("it"), 1)]);
    }
}
