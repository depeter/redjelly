//! Automatic subtitle synchronisation (SPEC §6.4): moves a subtitle's cues
//! onto a reference timing (another subtitle track, or speech spans detected
//! in the audio).
//!
//! The offset is piecewise constant: one per part of the film. Releases
//! differ by more than a shift: a director's cut adds, shortens and moves
//! scenes, so the offset steps up *and down* along the film. We find the best
//! offset path with Viterbi over a grid of offsets, where staying on an
//! offset is free and switching costs a fixed penalty. Unlike alass, lines
//! may be dropped or reordered, so a scene missing from one cut doesn't
//! derail everything around it.
//!
//! Cues are scored on how close their start and end land to a reference
//! cue's start and end. That separates the right offset from wrong ones far
//! better than overlap: both languages start a line when someone starts
//! talking, while a dense reference overlaps something almost anywhere.
//!
//! Framerate mismatches (25 vs 23.976 fps: a 4 % drift) are handled by
//! trying the common ratios and keeping the clearly best fit.
//!
//! Speech detected in the audio is a much noisier reference (music and
//! effects hide a lot of speech, spans merge several lines), and there the
//! edge scoring jumps to wrong offsets; alass's ordered alignment holds up
//! better, so `align_to_speech` uses that (measured on a real film: SPEC §6.4).

use alass_core::{NoProgressHandler, TimeDelta, TimePoint, TimeSpan};

/// Grid step of the coarse offset search (ms).
const STEP_MS: i64 = 100;

/// Half-width of the start/end matching kernel, coarse pass (ms).
const KERNEL_MS: i64 = 500;

/// Half-width of the kernel of the fine pass, and its step (ms).
const FINE_KERNEL_MS: i64 = 200;
const FINE_STEP_MS: i64 = 10;

/// Cost of switching offset, in "well-matched cues": a part needs about
/// this many more matching lines at its own offset to get one.
const PENALTY: f32 = 4.0;

/// Cues longer than this are signs, karaoke or "♪ music ♪" banners, not
/// speech: they only blur the alignment.
const MAX_CUE_MS: i64 = 12_000;

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
        1 + self.deltas.windows(2).filter(|w| (w[0] - w[1]).abs() > 300).count()
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

/// Cues that say something about speech timing, sorted.
fn usable(cues: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut v: Vec<_> = cues.iter().copied().filter(|&(s, e)| e > s && e - s <= MAX_CUE_MS && s >= 0).collect();
    v.sort_unstable();
    v
}

fn scaled(cues: &[(i64, i64)], scale: f64) -> Vec<(i64, i64)> {
    cues.iter().map(|&(s, e)| ((s as f64 * scale).round() as i64, (e as f64 * scale).round() as i64)).collect()
}

/// Reference cue edges, sorted, for kernel lookups.
struct Edges {
    starts: Vec<i64>,
    ends: Vec<i64>,
}

impl Edges {
    fn new(reference: &[(i64, i64)]) -> Self {
        let mut starts: Vec<i64> = reference.iter().map(|c| c.0).collect();
        let mut ends: Vec<i64> = reference.iter().map(|c| c.1).collect();
        starts.sort_unstable();
        ends.sort_unstable();
        Self { starts, ends }
    }

    /// Writes into `buf` (one cell per grid offset in `-range..=range`) a
    /// triangular kernel around every reference edge a target edge at `t`
    /// can reach, as a max per cell: one target edge scores at most 1.
    fn kernel(edges: &[i64], t: i64, range: i64, buf: &mut [f32]) {
        let n = buf.len() as i64;
        let w = KERNEL_MS / STEP_MS;
        let lo = edges.partition_point(|&x| x < t - range - KERNEL_MS);
        for &x in &edges[lo..] {
            let o = x - t;
            if o > range + KERNEL_MS {
                break;
            }
            let c = (o + range).div_euclid(STEP_MS);
            for k in (c - w).max(0)..=(c + w + 1).min(n - 1) {
                let d = (k * STEP_MS - range - o).abs();
                let v = 1.0 - d as f32 / KERNEL_MS as f32;
                if v > buf[k as usize] {
                    buf[k as usize] = v;
                }
            }
        }
    }

    /// Coarse-grid scores of a target cue's start and end for every offset.
    fn emissions(&self, (s, e): (i64, i64), range: i64, starts: &mut [f32], ends: &mut [f32]) {
        starts.iter_mut().for_each(|v| *v = 0.0);
        ends.iter_mut().for_each(|v| *v = 0.0);
        Self::kernel(&self.starts, s, range, starts);
        Self::kernel(&self.ends, e, range, ends);
    }

    /// Fine score of a target cue at offset `o`.
    fn fine(&self, (s, e): (i64, i64), o: i64) -> f32 {
        let near = |edges: &[i64], t: i64| {
            let i = edges.partition_point(|&x| x < t);
            let d = [i.checked_sub(1), Some(i)]
                .into_iter()
                .flatten()
                .filter_map(|j| edges.get(j))
                .map(|&x| (x - t).abs())
                .min()
                .unwrap_or(i64::MAX);
            (1.0 - d as f32 / FINE_KERNEL_MS as f32).max(0.0)
        };
        0.5 * (near(&self.starts, s + o) + near(&self.ends, e + o))
    }
}

fn argmax(v: &[f32]) -> (usize, f32) {
    v.iter().copied().enumerate().fold((0, f32::MIN), |a, (i, x)| if x > a.1 { (i, x) } else { a })
}

/// Viterbi over the offset grid; returns each cue's offset and the path score.
fn offset_path(edges: &Edges, target: &[(i64, i64)], range: i64) -> (Vec<i64>, f32) {
    let n_states = (2 * range / STEP_MS + 1) as usize;
    let words = n_states.div_ceil(64);
    let mut score = vec![0f32; n_states];
    let (mut starts, mut ends) = (vec![0f32; n_states], vec![0f32; n_states]);
    // Per cue: which states were reached by switching (bitset), and from where.
    let mut switched = vec![0u64; words * target.len()];
    let mut from = Vec::with_capacity(target.len());
    for (i, &cue) in target.iter().enumerate() {
        edges.emissions(cue, range, &mut starts, &mut ends);
        let (bi, bv) = argmax(&score);
        let jump = bv - PENALTY;
        let bits = &mut switched[i * words..(i + 1) * words];
        for k in 0..n_states {
            if jump > score[k] {
                score[k] = jump;
                bits[k / 64] |= 1 << (k % 64);
            }
            score[k] += 0.5 * (starts[k] + ends[k]);
        }
        from.push(bi);
    }
    let (mut k, total) = argmax(&score);
    let mut out = vec![0; target.len()];
    for i in (0..target.len()).rev() {
        out[i] = k as i64 * STEP_MS - range;
        if switched[i * words + k / 64] >> (k % 64) & 1 == 1 {
            k = from[i];
        }
    }
    (out, total)
}

/// Refines each run of equal coarse offsets to 10 ms.
fn refine(edges: &Edges, target: &[(i64, i64)], offsets: &mut [i64]) {
    let reach = (FINE_KERNEL_MS + STEP_MS) / FINE_STEP_MS;
    let mut start = 0;
    while start < offsets.len() {
        let mut end = start + 1;
        while end < offsets.len() && offsets[end] == offsets[start] {
            end += 1;
        }
        let c = offsets[start];
        let best = (-reach..=reach)
            .map(|j| c + j * FINE_STEP_MS)
            .map(|o| (o, target[start..end].iter().map(|&cue| edges.fine(cue, o)).sum::<f32>()))
            .fold((c, f32::MIN), |a, b| if b.1 > a.1 { b } else { a })
            .0;
        offsets[start..end].iter_mut().for_each(|d| *d = best);
        start = end;
    }
}

/// Every original cue (also skipped ones) takes the offset of the nearest
/// usable cue in time.
fn spread(target: &[(i64, i64)], clean: &[(i64, i64)], offsets: &[i64]) -> Vec<i64> {
    target
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
            offsets[pick]
        })
        .collect()
}

/// Moves `target` cues onto the timing of another subtitle track.
pub fn align(reference: &[(i64, i64)], target: &[(i64, i64)]) -> Alignment {
    let reference = usable(reference);
    let clean = usable(target);
    if reference.is_empty() || clean.is_empty() {
        return Alignment { scale: 1.0, deltas: vec![0; target.len()] };
    }
    let edges = Edges::new(&reference);
    // Wide enough for a director's cut or a long intro: 12 % of the film,
    // at least 5 and at most 20 minutes.
    let length = reference.iter().chain(&clean).map(|c| c.1).max().unwrap_or(0);
    let range = (length * 12 / 100).clamp(300_000, 1_200_000) / STEP_MS * STEP_MS;

    let mut best: Option<(f64, Vec<i64>, f32)> = None;
    for &ratio in &RATIOS {
        let (offsets, score) = offset_path(&edges, &scaled(&clean, ratio), range);
        // A different framerate has to win clearly, not by noise.
        let score = if ratio == 1.0 { score * 1.05 } else { score };
        if best.as_ref().is_none_or(|b| score > b.2) {
            best = Some((ratio, offsets, score));
        }
    }
    let (scale, mut offsets, _) = best.expect("RATIOS is not empty");
    refine(&edges, &scaled(&clean, scale), &mut offsets);
    Alignment { scale, deltas: spread(target, &clean, &offsets) }
}

/// alass works on integer time points; 10 ms is plenty precise.
const ALASS_UNIT_MS: i64 = 10;

/// Higher = fewer splits; alass recommends 7 ± 1, a little stricter keeps
/// voice detection noise from scattering the result.
const ALASS_SPLIT_PENALTY: f64 = 8.0;

fn alass_spans(cues: &[(i64, i64)], scale: f64) -> Vec<TimeSpan> {
    cues.iter()
        .map(|&(s, e)| {
            let s = (s as f64 * scale) as i64 / ALASS_UNIT_MS;
            let e = (e as f64 * scale) as i64 / ALASS_UNIT_MS;
            TimeSpan::new_safe(TimePoint::from(s), TimePoint::from(e.max(s + 1)))
        })
        .collect()
}

fn alass_split(reference: &[TimeSpan], target: &[(i64, i64)], scale: f64) -> (Vec<i64>, f64) {
    let spans = alass_spans(target, scale);
    let (deltas, score) =
        alass_core::align(reference, &spans, ALASS_SPLIT_PENALTY, Some(1.0), alass_core::standard_scoring, NoProgressHandler);
    (deltas.iter().map(|d: &TimeDelta| d.as_i64() * ALASS_UNIT_MS).collect(), score)
}

/// Moves `target` cues onto speech spans detected in the audio.
pub fn align_to_speech(speech: &[(i64, i64)], target: &[(i64, i64)]) -> Alignment {
    let reference = alass_spans(&usable(speech), 1.0);
    let clean = usable(target);
    if reference.is_empty() || clean.is_empty() {
        return Alignment { scale: 1.0, deltas: vec![0; target.len()] };
    }
    // The likeliest other framerate by a cheap single-shift fit; it has to
    // beat the native one clearly in the full alignment.
    let other = RATIOS[1..]
        .iter()
        .map(|&r| {
            let (_, score) = alass_core::align_nosplit(
                &reference,
                &alass_spans(&clean, r),
                alass_core::overlap_scoring,
                NoProgressHandler,
            );
            (r, score)
        })
        .fold((1.0, f64::MIN), |a, b| if b.1 > a.1 { b } else { a })
        .0;
    let (native, native_score) = alass_split(&reference, &clean, 1.0);
    let (other_offsets, other_score) = alass_split(&reference, &clean, other);
    let (scale, offsets) = if other_score > native_score * 1.05 { (other, other_offsets) } else { (1.0, native) };
    Alignment { scale, deltas: spread(target, &clean, &offsets) }
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

    /// `truth[i]` = where target cue `i` should start (None = nowhere).
    fn check(truth: &[Option<i64>], target: &[(i64, i64)], a: &Alignment, tolerance: i64) {
        assert_eq!(a.deltas.len(), target.len());
        let (mut bad, mut n) = (0, 0);
        for (i, (&(s, e), want)) in target.iter().zip(truth).enumerate() {
            let Some(want) = want else { continue };
            n += 1;
            if (a.map(i, s, e).0 - want).abs() > tolerance {
                bad += 1;
            }
        }
        assert!(bad * 100 <= n * 3, "{bad} of {n} cues off (scale {})", a.scale);
    }

    fn starts(v: &[(i64, i64)]) -> Vec<Option<i64>> {
        v.iter().map(|c| Some(c.0)).collect()
    }

    #[test]
    fn recovers_constant_offset() {
        let reference = pattern(200);
        let late: Vec<_> = reference.iter().map(|&(s, e)| (s + 2_300, e + 2_300)).collect();
        let a = align(&reference, &late);
        assert_eq!(a.scale, 1.0);
        let c = a.as_constant().expect("constant");
        assert!((c + 2_300).abs() <= 20, "shift {c}");
        check(&starts(&reference), &late, &a, 30);
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
        check(&starts(&reference), &pal, &a, 60);
    }

    #[test]
    fn handles_an_added_scene() {
        // The reference (director's cut) has 6 s more in the middle.
        let target = pattern(400);
        let reference: Vec<_> =
            target.iter().enumerate().map(|(i, &(s, e))| if i < 200 { (s, e) } else { (s + 6_000, e + 6_000) }).collect();
        let a = align(&reference, &target);
        assert!(a.sections() >= 2);
        check(&starts(&reference), &target, &a, 30);
    }

    #[test]
    fn handles_a_scene_missing_from_the_reference() {
        // Lines 150..190 of the target are a scene the reference doesn't
        // have, so the offset steps *down* afterwards: alass can't do this.
        let target = pattern(400);
        let cut = target[190].0 - target[150].0;
        let (mut reference, mut truth) = (Vec::new(), Vec::new());
        for (i, &(s, e)) in target.iter().enumerate() {
            match i {
                0..150 => {
                    reference.push((s + 30_000, e + 30_000));
                    truth.push(Some(s + 30_000));
                }
                150..190 => truth.push(None),
                _ => {
                    reference.push((s + 30_000 - cut, e + 30_000 - cut));
                    truth.push(Some(s + 30_000 - cut));
                }
            }
        }
        let a = align(&reference, &target);
        check(&truth, &target, &a, 30);
    }

    #[test]
    fn cross_language_merged_lines() {
        // The target (another language) often joins two reference lines.
        let reference = pattern(500);
        let (mut target, mut truth) = (Vec::new(), Vec::new());
        let mut i = 0;
        while i < reference.len() {
            let last = if i % 3 == 0 && i + 1 < reference.len() { i + 1 } else { i };
            target.push((reference[i].0 + 4_000, reference[last].1 + 4_000));
            truth.push(Some(reference[i].0));
            i = last + 1;
        }
        let a = align(&reference, &target);
        let c = a.as_constant().expect("constant");
        assert!((c + 4_000).abs() <= 20, "shift {c}");
        check(&truth, &target, &a, 30);
    }

    #[test]
    fn speech_constant_offset_and_framerate() {
        let speech = pattern(600);
        let late: Vec<_> = speech.iter().map(|&(s, e)| (s + 2_300, e + 2_300)).collect();
        let a = align_to_speech(&speech, &late);
        let c = a.as_constant().expect("constant");
        assert!((c + 2_300).abs() <= 20, "shift {c}");
        let pal: Vec<_> = speech
            .iter()
            .map(|&(s, e)| ((s as f64 * 23.976 / 25.0) as i64 + 800, (e as f64 * 23.976 / 25.0) as i64 + 800))
            .collect();
        let a = align_to_speech(&speech, &pal);
        assert!((a.scale - 25.0 / 23.976).abs() < 1e-9, "scale {}", a.scale);
        check(&starts(&speech), &pal, &a, 60);
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
