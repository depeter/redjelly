//! Minimal SentencePiece (unigram) tokenizer for Opus-MT `source.spm`
//! files: reads the model protobuf and segments text with Viterbi. Pure
//! Rust so the Windows cross-build needs no C++ tokenizer library.

use std::collections::HashMap;

use unicode_normalization::UnicodeNormalization;

pub const SPACE: char = '\u{2581}'; // "▁"

#[derive(Debug, Clone)]
pub struct SentencePiece {
    pieces: HashMap<String, f32>,
    max_len: usize,
    unk_score: f32,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid sentencepiece model: {0}")]
pub struct SpmError(&'static str);

/// Reads a protobuf varint at `*pos`.
fn varint(buf: &[u8], pos: &mut usize) -> Result<u64, SpmError> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *buf.get(*pos).ok_or(SpmError("truncated varint"))?;
        *pos += 1;
        v |= u64::from(b & 0x7f) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(SpmError("varint too long"))
}

/// (field number, wire type, payload)
type Field<'a> = (u64, u8, &'a [u8]);

/// Splits one protobuf message into its fields.
fn fields(buf: &[u8]) -> Result<Vec<Field<'_>>, SpmError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let key = varint(buf, &mut pos)?;
        let (field, wire) = (key >> 3, (key & 7) as u8);
        let start = pos;
        match wire {
            0 => {
                varint(buf, &mut pos)?;
            }
            1 => pos += 8,
            2 => {
                let len = varint(buf, &mut pos)? as usize;
                let start = pos;
                pos += len;
                if pos > buf.len() {
                    return Err(SpmError("truncated field"));
                }
                out.push((field, wire, &buf[start..pos]));
                continue;
            }
            5 => pos += 4,
            _ => return Err(SpmError("unsupported wire type")),
        }
        if pos > buf.len() {
            return Err(SpmError("truncated field"));
        }
        out.push((field, wire, &buf[start..pos]));
    }
    Ok(out)
}

impl SentencePiece {
    /// Parses a `.spm` model (ModelProto). Only normal and user-defined
    /// pieces take part in segmentation.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, SpmError> {
        let mut pieces = HashMap::new();
        let mut min_score = 0f32;
        for (field, wire, payload) in fields(buf)? {
            if field != 1 || wire != 2 {
                continue;
            }
            let (mut piece, mut score, mut kind) = (None, 0f32, 1u64);
            for (f, w, p) in fields(payload)? {
                match (f, w) {
                    (1, 2) => piece = std::str::from_utf8(p).ok().map(str::to_owned),
                    (2, 5) => score = f32::from_le_bytes(p.try_into().map_err(|_| SpmError("bad score"))?),
                    (3, 0) => kind = varint(p, &mut 0)?,
                    _ => {}
                }
            }
            min_score = min_score.min(score);
            if let (Some(piece), 1 | 4) = (piece, kind) {
                pieces.insert(piece, score);
            }
        }
        if pieces.is_empty() {
            return Err(SpmError("no pieces"));
        }
        let max_len = pieces.keys().map(|p| p.chars().count()).max().unwrap_or(1);
        Ok(Self { pieces, max_len, unk_score: min_score - 10.0 })
    }

    #[cfg(test)]
    fn from_pieces(list: &[(&str, f32)]) -> Self {
        let pieces: HashMap<String, f32> = list.iter().map(|(p, s)| (p.to_string(), *s)).collect();
        let max_len = pieces.keys().map(|p| p.chars().count()).max().unwrap_or(1);
        let min = list.iter().map(|x| x.1).fold(0f32, f32::min);
        Self { pieces, max_len, unk_score: min - 10.0 }
    }

    /// NFKC, whitespace collapsed, words marked with "▁" (SentencePiece's
    /// default "add dummy prefix" behaviour).
    fn normalize(text: &str) -> String {
        let nfkc: String = text.nfkc().collect();
        let mut out = String::new();
        for word in nfkc.split_whitespace() {
            out.push(SPACE);
            out.push_str(word);
        }
        out
    }

    /// Splits text into the most likely sequence of pieces. Characters no
    /// piece covers become single-character pieces (mapped to `<unk>` later).
    pub fn encode(&self, text: &str) -> Vec<String> {
        let chars: Vec<char> = Self::normalize(text).chars().collect();
        let n = chars.len();
        let mut best = vec![f32::NEG_INFINITY; n + 1];
        let mut back = vec![0usize; n + 1];
        best[0] = 0.0;
        let mut buf = String::new();
        for end in 1..=n {
            for start in end.saturating_sub(self.max_len)..end {
                if best[start] == f32::NEG_INFINITY {
                    continue;
                }
                buf.clear();
                buf.extend(&chars[start..end]);
                let score = match self.pieces.get(&buf) {
                    Some(s) => *s,
                    None if end - start == 1 => self.unk_score,
                    None => continue,
                };
                if best[start] + score > best[end] {
                    best[end] = best[start] + score;
                    back[end] = start;
                }
            }
        }
        let mut out = Vec::new();
        let mut end = n;
        while end > 0 {
            let start = back[end];
            out.push(chars[start..end].iter().collect());
            end = start;
        }
        out.reverse();
        out
    }
}

/// Joins decoded pieces back into text.
pub fn detokenize<'a>(pieces: impl IntoIterator<Item = &'a str>) -> String {
    pieces.into_iter().collect::<String>().replace(SPACE, " ").trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_high_scoring_pieces() {
        let spm = SentencePiece::from_pieces(&[
            ("▁hello", -1.0),
            ("▁he", -2.0),
            ("llo", -2.0),
            ("▁world", -1.5),
            ("▁", -3.0),
            ("w", -5.0),
        ]);
        assert_eq!(spm.encode("hello   world"), ["▁hello", "▁world"]);
        assert_eq!(spm.encode("hellox"), ["▁hello", "x"]);
    }

    #[test]
    fn parses_protobuf() {
        // ModelProto { pieces: [{piece:"▁a", score:-1.5, type:NORMAL}, {piece:"<unk>", type:UNKNOWN}] }
        fn piece(p: &str, score: f32, kind: u8) -> Vec<u8> {
            let mut m = vec![0x0a, p.len() as u8];
            m.extend(p.as_bytes());
            m.push(0x15);
            m.extend(score.to_le_bytes());
            m.extend([0x18, kind]);
            let mut out = vec![0x0a, m.len() as u8];
            out.extend(m);
            out
        }
        let mut buf = piece("▁a", -1.5, 1);
        buf.extend(piece("<unk>", 0.0, 2));
        let spm = SentencePiece::from_bytes(&buf).unwrap();
        assert_eq!(spm.pieces.len(), 1);
        assert_eq!(spm.pieces["▁a"], -1.5);
    }

    #[test]
    fn detokenizes() {
        assert_eq!(detokenize(["▁Hal", "lo", "▁wereld", "."]), "Hallo wereld.");
    }
}
