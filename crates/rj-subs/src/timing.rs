//! Reading and rewriting cue timestamps in SRT, WebVTT and ASS files,
//! leaving everything else (text, styling, formatting) byte-for-byte intact.

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Srt,
    Vtt,
    Ass,
}

impl Format {
    pub fn from_ext(ext: &str) -> Option<Self> {
        match ext {
            "srt" => Some(Self::Srt),
            "vtt" => Some(Self::Vtt),
            "ass" | "ssa" => Some(Self::Ass),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start_ms: i64,
    pub end_ms: i64,
    start_at: Range<usize>,
    end_at: Range<usize>,
}

/// `HH:MM:SS,mmm` / `HH:MM:SS.mmm` / `MM:SS.mmm` (SRT, VTT).
fn parse_clock(s: &str) -> Option<i64> {
    let (hms, frac) = s.rsplit_once([',', '.'])?;
    let frac_ms = match frac.len() {
        3 => frac.parse::<i64>().ok()?,
        2 => frac.parse::<i64>().ok()? * 10,
        1 => frac.parse::<i64>().ok()? * 100,
        _ => return None,
    };
    let parts: Vec<i64> = hms.split(':').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
    let secs = match parts.as_slice() {
        [h, m, s] => h * 3600 + m * 60 + s,
        [m, s] => m * 60 + s,
        _ => return None,
    };
    Some(secs * 1000 + frac_ms)
}

fn format_clock(ms: i64, format: Format) -> String {
    let ms = ms.max(0);
    let (h, m, s, f) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    match format {
        Format::Srt => format!("{h:02}:{m:02}:{s:02},{f:03}"),
        Format::Vtt => format!("{h:02}:{m:02}:{s:02}.{f:03}"),
        // ASS uses centiseconds and a single-digit hour.
        Format::Ass => format!("{h}:{m:02}:{s:02}.{:02}", f / 10),
    }
}

/// Every cue with its timestamp positions in `text`.
pub fn parse(text: &str, format: Format) -> Vec<Cue> {
    let mut cues = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let base = offset;
        offset += line.len();
        let line = line.trim_end_matches(['\n', '\r']);
        match format {
            Format::Srt | Format::Vtt => {
                let Some(arrow) = line.find("-->") else { continue };
                let left = &line[..arrow];
                let right_full = &line[arrow + 3..];
                let lstart = left.len() - left.trim_start().len();
                let ltok = left.trim();
                let rstart = right_full.len() - right_full.trim_start().len();
                // VTT may follow the end time with cue settings.
                let rtok = right_full.split_whitespace().next().unwrap_or("");
                let (Some(start_ms), Some(end_ms)) = (parse_clock(ltok), parse_clock(rtok)) else { continue };
                let s0 = base + lstart;
                let e0 = base + arrow + 3 + rstart;
                cues.push(Cue { start_ms, end_ms, start_at: s0..s0 + ltok.len(), end_at: e0..e0 + rtok.len() });
            }
            Format::Ass => {
                let Some(rest) = line.strip_prefix("Dialogue:") else { continue };
                // Fields: Layer, Start, End, ...
                let fields: Vec<&str> = rest.splitn(4, ',').collect();
                if fields.len() < 4 {
                    continue;
                }
                let field_start = |i: usize| -> usize {
                    base + "Dialogue:".len() + fields[..i].iter().map(|f| f.len() + 1).sum::<usize>()
                };
                let (st, et) = (fields[1], fields[2]);
                let (Some(start_ms), Some(end_ms)) = (parse_clock(st.trim()), parse_clock(et.trim())) else { continue };
                let s0 = field_start(1) + (st.len() - st.trim_start().len());
                let e0 = field_start(2) + (et.len() - et.trim_start().len());
                cues.push(Cue { start_ms, end_ms, start_at: s0..s0 + st.trim().len(), end_at: e0..e0 + et.trim().len() });
            }
        }
    }
    cues
}

/// Applies per-cue offsets (`deltas[i]` ms for `cues[i]`).
pub fn shift(text: &str, format: Format, cues: &[Cue], deltas: &[i64]) -> String {
    let mut edits: Vec<(Range<usize>, String)> = Vec::with_capacity(cues.len() * 2);
    for (cue, d) in cues.iter().zip(deltas) {
        edits.push((cue.start_at.clone(), format_clock(cue.start_ms + d, format)));
        edits.push((cue.end_at.clone(), format_clock(cue.end_ms + d, format)));
    }
    edits.sort_by_key(|(r, _)| r.start);
    let mut out = String::with_capacity(text.len() + 64);
    let mut pos = 0;
    for (range, new) in edits {
        out.push_str(&text[pos..range.start]);
        out.push_str(&new);
        pos = range.end;
    }
    out.push_str(&text[pos..]);
    out
}

/// Shifts every cue by the same amount.
pub fn shift_all(text: &str, format: Format, delta_ms: i64) -> String {
    let cues = parse(text, format);
    let deltas = vec![delta_ms; cues.len()];
    shift(text, format, &cues, &deltas)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRT: &str = "1\r\n00:00:01,500 --> 00:00:03,000\r\nHallo\r\n\r\n2\r\n01:02:03,004 --> 01:02:05,000\r\nDoei\r\n";

    #[test]
    fn parses_srt() {
        let cues = parse(SRT, Format::Srt);
        assert_eq!(cues.len(), 2);
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (1500, 3000));
        assert_eq!(cues[1].start_ms, 3_723_004);
    }

    #[test]
    fn shifts_srt_keeping_everything_else() {
        let out = shift_all(SRT, Format::Srt, 250);
        assert_eq!(out, SRT.replace("00:00:01,500", "00:00:01,750").replace("00:00:03,000", "00:00:03,250")
            .replace("01:02:03,004", "01:02:03,254").replace("01:02:05,000", "01:02:05,250"));
    }

    #[test]
    fn vtt_with_settings_and_short_clock() {
        let vtt = "WEBVTT\n\n00:01.000 --> 00:02.500 align:start\nHi\n";
        let cues = parse(vtt, Format::Vtt);
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (1000, 2500));
        assert_eq!(shift_all(vtt, Format::Vtt, -500), "WEBVTT\n\n00:00:00.500 --> 00:00:02.000 align:start\nHi\n");
    }

    #[test]
    fn ass_dialogue_lines() {
        let ass = "[Events]\nFormat: Layer, Start, End, Style, Text\nDialogue: 0,0:00:01.50,0:00:03.00,Default,,0,0,0,,Hallo, wereld\n";
        let cues = parse(ass, Format::Ass);
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (1500, 3000));
        let out = shift_all(ass, Format::Ass, 1000);
        assert!(out.contains("Dialogue: 0,0:00:02.50,0:00:04.00,Default,,0,0,0,,Hallo, wereld"), "{out}");
    }

    #[test]
    fn negative_shift_clamps_at_zero() {
        let out = shift_all(SRT, Format::Srt, -5000);
        assert!(out.contains("00:00:00,000 --> 00:00:00,000"));
    }
}
