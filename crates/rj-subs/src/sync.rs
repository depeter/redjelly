//! Automatic subtitle synchronisation with alass (SPEC §6.4): aligns a
//! subtitle's cues to a reference timing (another subtitle track, or speech
//! spans detected in the audio).
//!
//! Like the alass CLI it works in two passes: first a framerate check
//! (subtitles made for a 25 fps PAL release drift ~4 % against a 23.976 fps
//! one, which no constant shift can fix), then a split alignment that may give
//! different parts different offsets (ads or scenes cut from one release).

use alass_core::{NoProgressHandler, TimeDelta, TimePoint, TimeSpan};

/// alass works on integer time points; 10 ms is plenty precise and keeps
/// alignment of a 2 h film fast.
const UNIT_MS: i64 = 10;

/// Cues longer than this are signs, karaoke or "♪ music ♪" banners, not
/// speech: they only blur the alignment.
const MAX_CUE_MS: i64 = 12_000;

/// Higher = fewer splits. alass recommends 7 ± 1; a little stricter keeps
/// noisy references (voice detection) from scattering the result.
const SPLIT_PENALTY: f64 = 8.0;

/// Framerate ratios to try (target time × ratio).
const RATIOS: [f64; 7] = [
    1.0,
    25.0 / 23.976,
    23.976 / 25.0,
    25.0 / 24.0,
    24.0 / 25.0,
    24.0 / 23.976,
    23.976 / 24.0,
];

/// How a target subtitle maps onto the reference.
#[derive(Debug, Clone, PartialEq)]
pub struct Alignment {
    /// Framerate correction applied before the per-cue offsets (1.0 = none).
    pub scale: f64,
    /// Offset (ms) for each target cue, after scaling.
    pub deltas: Vec<i64>,
}

impl Alignment {
    /// New (start, end) of cue `i`.
    pub fn map(&self, i: usize, start: i64, end: i64) -> (i64, i64) {
        let d = self.deltas.get(i).copied().unwrap_or(0);
        ((start as f64 * self.scale).round() as i64 + d, (end as f64 * self.scale).round() as i64 + d)
    }

    pub fn framerate_fixed(&self) -> bool {
        self.scale != 1.0
    }

    /// Median offset (ms): the "how far was it off" number for the user.
    pub fn median_shift(&self) -> i64 {
        let mut v = self.deltas.clone();
        v.sort_unstable();
        v.get(v.len() / 2).copied().unwrap_or(0)
    }

    /// Number of parts with a different offset.
    pub fn sections(&self) -> usize {
        if self.deltas.is_empty() {
            return 0;
        }
        1 + self.deltas.windows(2).filter(|w| (w[0] - w[1]).abs() > 100).count()
    }

    /// The single shift that does the job when no rescaling is needed and
    /// nearly every cue moves by (about) the same amount, so a player delay
    /// is enough and no rewritten file is needed.
    pub fn as_constant(&self) -> Option<i64> {
        if self.framerate_fixed() || self.deltas.is_empty() {
            return None;
        }
        let m = self.median_shift();
        let close = self.deltas.iter().filter(|d| (**d - m).abs() <= 150).count();
        (close * 100 >= self.deltas.len() * 97).then_some(m)
    }
}

fn to_span(s: i64, e: i64) -> TimeSpan {
    let s = s.max(0) / UNIT_MS;
    TimeSpan::new_safe(TimePoint::from(s), TimePoint::from((e.max(0) / UNIT_MS).max(s + 1)))
}

/// Cues that say something about speech timing.
fn usable(cues: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut v: Vec<_> = cues.iter().copied().filter(|&(s, e)| e > s && e - s <= MAX_CUE_MS && s >= 0).collect();
    v.sort_unstable();
    v
}

fn scaled_spans(cues: &[(i64, i64)], scale: f64) -> Vec<TimeSpan> {
    cues.iter().map(|&(s, e)| to_span((s as f64 * scale) as i64, (e as f64 * scale) as i64)).collect()
}

/// The non-1 framerate ratio under which `target` fits `reference` best with
/// a single shift (cheap; the final choice compares full alignments).
fn best_other_scale(reference: &[TimeSpan], target: &[(i64, i64)]) -> f64 {
    let mut best = (1.0, f64::MIN);
    for &r in &RATIOS[1..] {
        let spans = scaled_spans(target, r);
        let (_, score) = alass_core::align_nosplit(reference, &spans, alass_core::overlap_scoring, NoProgressHandler);
        if score > best.1 {
            best = (r, score);
        }
    }
    best.0
}

fn split_align(reference: &[TimeSpan], target: &[(i64, i64)], scale: f64) -> (Vec<i64>, f64) {
    let spans = scaled_spans(target, scale);
    let (deltas, score) =
        alass_core::align(reference, &spans, SPLIT_PENALTY, Some(1.0), alass_core::standard_scoring, NoProgressHandler);
    (deltas.iter().map(|d: &TimeDelta| d.as_i64() * UNIT_MS).collect(), score)
}

/// Moves `target` cues onto `reference` timing.
pub fn align(reference: &[(i64, i64)], target: &[(i64, i64)]) -> Alignment {
    let reference: Vec<TimeSpan> = usable(reference).iter().map(|&(s, e)| to_span(s, e)).collect();
    let clean = usable(target);
    if reference.is_empty() || clean.is_empty() {
        return Alignment { scale: 1.0, deltas: vec![0; target.len()] };
    }
    // Split alignment on the usable cues, at the native framerate and at the
    // most likely other one; a framerate change has to win clearly.
    let (native, native_score) = split_align(&reference, &clean, 1.0);
    let other = best_other_scale(&reference, &clean);
    let (other_deltas, other_score) = split_align(&reference, &clean, other);
    let (scale, deltas) =
        if other_score > native_score * 1.05 { (other, other_deltas) } else { (1.0, native) };

    // Every original cue (also skipped ones) takes the offset of the
    // nearest usable cue in time.
    let deltas = target
        .iter()
        .map(|&(s, _)| {
            let i = clean.partition_point(|&(cs, _)| cs < s);
            let pick = match (i.checked_sub(1), (i < clean.len()).then_some(i)) {
                (Some(a), Some(b)) => {
                    if s - clean[a].0 <= clean[b].0 - s {
                        a
                    } else {
                        b
                    }
                }
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => return 0,
            };
            deltas[pick]
        })
        .collect();
    Alignment { scale, deltas }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Irregular, dialogue-like cue pattern over ~`n` × 4 s.
    fn pattern(n: i64) -> Vec<(i64, i64)> {
        (0..n)
            .map(|i| {
                let start = i * 4_000 + (i * i * 37 % 1_500);
                (start, start + 1_200 + (i * 13 % 900))
            })
            .collect()
    }

    fn check(reference: &[(i64, i64)], target: &[(i64, i64)], a: &Alignment, tolerance: i64) {
        assert_eq!(a.deltas.len(), target.len());
        let mut bad = 0;
        for (i, (&(s, e), &(rs, _))) in target.iter().zip(reference).enumerate() {
            let (ns, _) = a.map(i, s, e);
            if (ns - rs).abs() > tolerance {
                bad += 1;
            }
        }
        assert!(bad * 100 <= target.len() * 3, "{bad} of {} cues off (scale {})", target.len(), a.scale);
    }

    #[test]
    fn recovers_constant_offset() {
        let reference = pattern(200);
        let late: Vec<_> = reference.iter().map(|&(s, e)| (s + 2_300, e + 2_300)).collect();
        let a = align(&reference, &late);
        assert_eq!(a.scale, 1.0);
        let c = a.as_constant().expect("constant");
        assert!((c + 2_300).abs() <= 20, "shift {c}");
        check(&reference, &late, &a, 30);
    }

    #[test]
    fn corrects_pal_framerate() {
        // Subtitle timed for a 25 fps release, played against 23.976 fps:
        // its times are 23.976/25 of what they should be, plus an offset.
        let reference = pattern(600);
        let pal: Vec<_> = reference
            .iter()
            .map(|&(s, e)| ((s as f64 * 23.976 / 25.0) as i64 + 800, (e as f64 * 23.976 / 25.0) as i64 + 800))
            .collect();
        let a = align(&reference, &pal);
        assert!((a.scale - 25.0 / 23.976).abs() < 1e-9, "scale {}", a.scale);
        assert!(a.as_constant().is_none());
        check(&reference, &pal, &a, 60);
    }

    #[test]
    fn handles_a_cut_scene() {
        // Second half of the target is 6 s later (an extra scene in that release).
        let reference = pattern(400);
        let target: Vec<_> =
            reference.iter().enumerate().map(|(i, &(s, e))| if i < 200 { (s, e) } else { (s + 6_000, e + 6_000) }).collect();
        let a = align(&reference, &target);
        assert!(a.sections() >= 2);
        check(&reference, &target, &a, 30);
    }

    #[test]
    fn ignores_long_sign_cues() {
        let reference = pattern(200);
        let mut late: Vec<_> = reference.iter().map(|&(s, e)| (s + 1_000, e + 1_000)).collect();
        late.push((0, 600_000)); // a ten-minute sign
        let a = align(&reference, &late);
        let c = a.as_constant().expect("constant");
        assert!((c + 1_000).abs() <= 20, "shift {c}");
    }
}
