//! Cue clean-up and SRT output (SPEC §7 step 4).

use crate::Cue;

pub const MAX_LINE: usize = 42;
pub const MIN_MS: u64 = 1_000;
pub const MAX_MS: u64 = 7_000;

/// Wraps text to at most two lines of `MAX_LINE`, breaking near the middle.
pub fn wrap(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= MAX_LINE {
        return text;
    }
    let mid = text.len() / 2;
    let split = text
        .match_indices(' ')
        .map(|(i, _)| i)
        .min_by_key(|i| i.abs_diff(mid));
    match split {
        Some(i) => format!("{}\n{}", &text[..i], &text[i + 1..]),
        None => text,
    }
}

/// Splits a cue that's on screen too long into parts at a natural break
/// (sentence end, else comma, else a space near the middle), sharing the
/// time by text length, so text stays up while it is spoken.
fn split_long(c: Cue) -> Vec<Cue> {
    let dur = c.end_ms.saturating_sub(c.start_ms);
    let text = c.text.split_whitespace().collect::<Vec<_>>().join(" ");
    if dur <= MAX_MS || text.chars().count() < 20 {
        return vec![Cue { text, ..c }];
    }
    let mid = text.len() / 2;
    let best = |pred: &dyn Fn(char) -> bool| {
        text.char_indices()
            .filter(|&(i, ch)| pred(ch) && i > text.len() / 5 && i < text.len() * 4 / 5)
            .map(|(i, ch)| i + ch.len_utf8())
            .min_by_key(|i| i.abs_diff(mid))
    };
    let Some(cut) = best(&|ch| matches!(ch, '.' | '?' | '!'))
        .or_else(|| best(&|ch| matches!(ch, ',' | ';' | ':')))
        .or_else(|| best(&|ch| ch == ' '))
    else {
        return vec![Cue { text, ..c }];
    };
    let (a, b) = (text[..cut].trim().to_owned(), text[cut..].trim().to_owned());
    let split_at = c.start_ms + dur * a.len() as u64 / (a.len() + b.len()).max(1) as u64;
    let mut out = split_long(Cue { start_ms: c.start_ms, end_ms: split_at, text: a });
    out.extend(split_long(Cue { start_ms: split_at, end_ms: c.end_ms, text: b }));
    out
}

/// Drops empty and repeated hallucination lines, splits over-long cues,
/// enforces min/max duration without overlapping the next cue, and wraps text.
pub fn clean(cues: Vec<Cue>) -> Vec<Cue> {
    let cues: Vec<Cue> = cues.into_iter().flat_map(split_long).collect();
    let mut out: Vec<Cue> = Vec::with_capacity(cues.len());
    for c in cues {
        let text = c.text.trim();
        if text.is_empty() {
            continue;
        }
        // Whisper loops on silence: the same line three times in a row.
        let repeats = out.iter().rev().take(2).filter(|p| p.text.replace('\n', " ") == text).count();
        if repeats == 2 {
            continue;
        }
        out.push(Cue { text: text.to_owned(), ..c });
    }
    for i in 0..out.len() {
        let next_start = out.get(i + 1).map_or(u64::MAX, |n| n.start_ms);
        let c = &mut out[i];
        c.end_ms = c.end_ms.clamp(c.start_ms + MIN_MS, c.start_ms + MAX_MS).min(next_start.saturating_sub(1).max(c.start_ms + 1));
        c.text = wrap(&c.text);
    }
    out
}

fn ts(ms: u64) -> String {
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

fn parse_ts(s: &str) -> Option<u64> {
    let (hms, ms) = s.trim().split_once([',', '.'])?;
    let mut parts = hms.split(':').map(|p| p.parse::<u64>().ok());
    let (h, m, sec) = (parts.next()??, parts.next()??, parts.next()??);
    Some(h * 3_600_000 + m * 60_000 + sec * 1000 + ms.parse::<u64>().ok()?)
}

/// Reads an SRT file (as served by Jellyfin) into cues.
pub fn parse_srt(text: &str) -> Vec<Cue> {
    let text = text.replace("\r\n", "\n");
    let mut cues = Vec::new();
    for block in text.split("\n\n") {
        let mut lines = block.lines().skip_while(|l| !l.contains("-->"));
        let Some(times) = lines.next() else { continue };
        let Some((a, b)) = times.split_once("-->") else { continue };
        let (Some(start_ms), Some(end_ms)) = (parse_ts(a), parse_ts(b.split_whitespace().next().unwrap_or(""))) else {
            continue;
        };
        let body: Vec<&str> = lines.collect();
        if !body.is_empty() {
            cues.push(Cue { start_ms, end_ms, text: body.join("\n") });
        }
    }
    cues
}

pub fn to_srt(cues: &[Cue]) -> String {
    cues.iter()
        .enumerate()
        .map(|(i, c)| format!("{}\n{} --> {}\n{}\n", i + 1, ts(c.start_ms), ts(c.end_ms), c.text))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start_ms: u64, end_ms: u64, text: &str) -> Cue {
        Cue { start_ms, end_ms, text: text.into() }
    }

    #[test]
    fn formats_srt() {
        let s = to_srt(&[cue(1_500, 3_000, "Hallo"), cue(3_661_001, 3_662_000, "Doei")]);
        assert_eq!(s, "1\n00:00:01,500 --> 00:00:03,000\nHallo\n\n2\n01:01:01,001 --> 01:01:02,000\nDoei\n");
    }

    #[test]
    fn parses_srt_round_trip() {
        let cues = vec![cue(1_500, 3_000, "Hallo\nwereld"), cue(4_000, 5_000, "Doei")];
        assert_eq!(parse_srt(&to_srt(&cues).replace('\n', "\r\n")), cues);
    }

    #[test]
    fn wraps_long_lines_in_two() {
        let w = wrap("Dit is een behoorlijk lange zin die echt over twee regels moet");
        let lines: Vec<_> = w.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.len() <= MAX_LINE));
    }

    #[test]
    fn splits_long_cues_instead_of_cutting_them() {
        let out = clean(vec![cue(
            290,
            10_410,
            "And so, my fellow Americans, ask not what your country can do for you, ask what you can do for your country.",
        )]);
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0].start_ms, 290);
        assert_eq!(out[1].end_ms, 7_000.min(10_410 - out[1].start_ms) + out[1].start_ms);
        assert!(out[0].text.ends_with("for you,"), "{out:?}");
        assert!(out.iter().all(|c| c.end_ms - c.start_ms <= MAX_MS));
    }

    #[test]
    fn cleans_durations_and_hallucinations() {
        let out = clean(vec![
            cue(0, 100, "Kort"),
            cue(500, 20_000, "Lang"),
            cue(30_000, 31_000, "Bedankt voor het kijken"),
            cue(31_000, 32_000, "Bedankt voor het kijken"),
            cue(32_000, 33_000, "Bedankt voor het kijken"),
            cue(40_000, 41_000, "  "),
        ]);
        assert_eq!(out.len(), 4);
        assert_eq!(out[0].end_ms, 499); // min duration capped by next cue
        assert_eq!(out[1].end_ms, 7_500); // max duration
    }
}
