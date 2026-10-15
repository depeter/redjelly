//! Automatic subtitle synchronisation with alass (SPEC §6.4): aligns a
//! subtitle's cues to a reference timing (another subtitle track now; audio
//! voice activity once audio extraction lands in M3).

use alass_core::{NoProgressHandler, TimePoint, TimeSpan};

/// alass works on integer time points; 10 ms is plenty precise and keeps
/// alignment of a 2 h film well under a second.
const UNIT_MS: i64 = 10;

fn spans(cues: &[(i64, i64)]) -> Vec<TimeSpan> {
    cues.iter()
        .map(|&(s, e)| TimeSpan::new_safe(TimePoint::from(s / UNIT_MS), TimePoint::from(e.max(s + UNIT_MS) / UNIT_MS)))
        .collect()
}

/// Per-cue offsets (ms) that move `target` onto `reference`. Allows
/// different offsets in different parts (e.g. ads cut from one release).
pub fn align(reference: &[(i64, i64)], target: &[(i64, i64)]) -> Vec<i64> {
    let (deltas, _score) = alass_core::align(
        &spans(reference),
        &spans(target),
        7.0,
        Some(3.0),
        alass_core::standard_scoring,
        NoProgressHandler,
    );
    deltas.into_iter().map(|d| d.as_i64() * UNIT_MS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_constant_offset() {
        // Irregular cue pattern, then the same pattern 2.3 s late.
        let reference: Vec<(i64, i64)> = (0..200)
            .map(|i| {
                let start = i * 4_000 + (i * i * 37 % 1_500);
                (start, start + 1_200 + (i * 13 % 900))
            })
            .collect();
        let late: Vec<(i64, i64)> = reference.iter().map(|&(s, e)| (s + 2_300, e + 2_300)).collect();
        let deltas = align(&reference, &late);
        assert_eq!(deltas.len(), late.len());
        let median = {
            let mut d = deltas.clone();
            d.sort();
            d[d.len() / 2]
        };
        assert!((median + 2_300).abs() <= 20, "median delta {median}");
    }
}
