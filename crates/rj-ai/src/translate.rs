//! Opus-MT (MarianMT) translation on CPU via candle, using the Helsinki-NLP
//! files `config.json`, `model.safetensors`, `source.spm`, `vocab.json`
//! (e.g. Helsinki-NLP/opus-mt-en-nl; the original `pytorch_model.bin` is the
//! legacy torch format candle can't read, the safetensors conversion is).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::marian;
use rj_core::lang::Lang;

use crate::Error;
use crate::pipeline::Translator;
use crate::spm::{self, SentencePiece};

/// Longest output per line, in tokens (subtitle lines are short).
const MAX_OUTPUT: usize = 128;

pub struct MarianPair {
    from: Lang,
    to: Lang,
    model: marian::MTModel,
    config: marian::Config,
    spm: SentencePiece,
    vocab: HashMap<String, u32>,
    pieces: Vec<String>,
    unk: u32,
    device: Device,
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Translate(e.to_string())
}

impl MarianPair {
    /// Loads a model directory for the `from`→`to` pair.
    pub fn load(dir: &Path, from: Lang, to: Lang) -> Result<Self, Error> {
        let read = |name: &str| std::fs::read(dir.join(name)).map_err(|e| Error::ModelMissing(format!("{name} in {}: {e}", dir.display())));
        let config: marian::Config = serde_json::from_slice(&read("config.json")?).map_err(err)?;
        let vocab: HashMap<String, u32> = serde_json::from_slice(&read("vocab.json")?).map_err(err)?;
        let spm = SentencePiece::from_bytes(&read("source.spm")?).map_err(err)?;
        let mut pieces = vec![String::new(); vocab.values().max().map_or(0, |m| *m as usize + 1)];
        for (p, &i) in &vocab {
            pieces[i as usize] = p.clone();
        }
        let unk = *vocab.get("<unk>").ok_or_else(|| err("vocab has no <unk>"))?;
        let device = Device::Cpu;
        // SAFETY: the file is only mapped read-only and not modified while loaded.
        let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[dir.join("model.safetensors")], DType::F32, &device) }
            .map_err(err)?;
        let model = marian::MTModel::new(&config, vb).map_err(err)?;
        Ok(Self { from, to, model, config, spm, vocab, pieces, unk, device })
    }

    fn translate_line(&mut self, text: &str) -> Result<String, Error> {
        if text.trim().is_empty() {
            return Ok(String::new());
        }
        let mut ids: Vec<u32> =
            self.spm.encode(text).iter().map(|p| self.vocab.get(p).copied().unwrap_or(self.unk)).collect();
        ids.truncate(self.config.max_position_embeddings - 2);
        ids.push(self.config.eos_token_id);

        let input = Tensor::new(ids.as_slice(), &self.device).and_then(|t| t.unsqueeze(0)).map_err(err)?;
        let encoded = self.model.encoder().forward(&input, 0).map_err(err)?;

        let mut out = vec![self.config.decoder_start_token_id];
        for step in 0..MAX_OUTPUT {
            let context = if step == 0 { out.len() } else { 1 };
            let start = out.len() - context;
            let next = (|| {
                let input = Tensor::new(&out[start..], &self.device)?.unsqueeze(0)?;
                let logits = self.model.decode(&input, &encoded, start)?.squeeze(0)?;
                let last = logits.get(logits.dim(0)? - 1)?;
                // Greedy, never choosing padding (Marian's "bad word").
                let mut v: Vec<f32> = last.to_vec1()?;
                v[self.config.pad_token_id as usize] = f32::NEG_INFINITY;
                Ok::<u32, candle_core::Error>(
                    v.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |(i, _)| i as u32),
                )
            })()
            .map_err(err)?;
            if next == self.config.eos_token_id || next == self.config.forced_eos_token_id {
                break;
            }
            out.push(next);
        }
        self.model.reset_kv_cache();
        let text = spm::detokenize(
            out[1..]
                .iter()
                .filter_map(|&i| self.pieces.get(i as usize))
                .map(String::as_str)
                .filter(|p| !p.starts_with('<')),
        );
        Ok(text)
    }
}

/// All installed language pairs.
pub struct OpusMt {
    pairs: Vec<MarianPair>,
}

impl OpusMt {
    pub fn new(pairs: Vec<MarianPair>) -> Self {
        Self { pairs }
    }
}

impl Translator for OpusMt {
    fn supports(&self, from: &Lang, to: &Lang) -> bool {
        self.pairs.iter().any(|p| &p.from == from && &p.to == to)
    }

    fn translate(
        &mut self,
        lines: &[String],
        from: &Lang,
        to: &Lang,
        progress: &mut dyn FnMut(f32),
        cancel: &AtomicBool,
    ) -> Result<Vec<String>, Error> {
        let pair = self
            .pairs
            .iter_mut()
            .find(|p| &p.from == from && &p.to == to)
            .ok_or_else(|| Error::Translate(format!("no {from}→{to} model installed")))?;
        let mut out = Vec::with_capacity(lines.len());
        for (i, line) in lines.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            out.push(pair.translate_line(line)?);
            if i % 10 == 0 {
                progress(i as f32 / lines.len() as f32);
            }
        }
        progress(1.0);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the real model: `RJ_OPUS_EN_NL=<dir> cargo test -p rj-ai --features translate -- --ignored`
    #[test]
    #[ignore]
    fn translates_english_to_dutch() {
        let dir = std::env::var("RJ_OPUS_EN_NL").expect("set RJ_OPUS_EN_NL");
        let (en, nl) = (Lang::parse("en").unwrap(), Lang::parse("nl").unwrap());
        let mut t = OpusMt::new(vec![MarianPair::load(Path::new(&dir), en.clone(), nl.clone()).unwrap()]);
        let lines = vec![
            "And so, my fellow Americans, ask not what your country can do for you,".to_owned(),
            "ask what you can do for your country.".to_owned(),
            "Where were you last night?".to_owned(),
        ];
        let start = std::time::Instant::now();
        let out = t.translate(&lines, &en, &nl, &mut |_| {}, &AtomicBool::new(false)).unwrap();
        println!("{:?} in {:?}", out, start.elapsed());
        assert!(out[1].to_lowercase().contains("land"), "{out:?}");
        assert!(out[2].to_lowercase().contains("gisteravond") || out[2].to_lowercase().contains("nacht"), "{out:?}");
    }
}
