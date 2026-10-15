//! Orders candidates (SPEC §6.2): preferred language first, then free
//! providers before quota-limited ones (OpenSubtitles), hash match,
//! release-name similarity, hearing-impaired preference, popularity.

use std::cmp::Reverse;
use std::collections::HashSet;

use rj_core::tracks::Preferences;

use crate::provider::{Candidate, Query};

fn tokens(s: &str) -> HashSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 1)
        .map(str::to_lowercase)
        .collect()
}

/// Number of release tokens (group, source, resolution…) shared with the file name.
pub fn release_similarity(file_name: &str, release: &str) -> usize {
    tokens(file_name).intersection(&tokens(release)).count()
}

pub fn rank(candidates: &mut [Candidate], q: &Query, prefs: &Preferences) {
    let file = q.file_name.as_deref().unwrap_or("");
    candidates.sort_by_cached_key(|c| {
        let lang_rank = prefs.languages.iter().position(|l| *l == c.lang).unwrap_or(usize::MAX);
        (
            lang_rank,
            c.provider.costs_quota(),
            Reverse(c.hash_match),
            Reverse(release_similarity(file, &c.release)),
            c.hearing_impaired != prefs.prefer_sdh,
            Reverse(c.downloads),
        )
    });
}

#[cfg(test)]
mod tests {
    use rj_core::lang::Lang;

    use super::*;
    use crate::provider::ProviderId;

    fn cand(id: &str, lang: &str, release: &str, hash: bool) -> Candidate {
        Candidate {
            provider: ProviderId::OpenSubtitles,
            id: id.into(),
            lang: Lang::parse(lang).unwrap(),
            release: release.into(),
            format: None,
            hash_match: hash,
            hearing_impaired: false,
            forced: false,
            downloads: 0,
            rating: None,
        }
    }

    #[test]
    fn free_providers_first() {
        let prefs = Preferences { languages: vec![Lang::parse("nl").unwrap()], prefer_sdh: false };
        let mut os = cand("os", "nl", "x", true);
        os.provider = ProviderId::OpenSubtitles;
        let mut free = cand("subdl", "nl", "y", false);
        free.provider = ProviderId::Subdl;
        let mut c = vec![os, free];
        rank(&mut c, &Query::default(), &prefs);
        assert_eq!(c[0].id, "subdl");
    }

    #[test]
    fn language_then_hash_then_release() {
        let q = Query { file_name: Some("Show.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb.mkv".into()), ..Default::default() };
        let prefs = Preferences { languages: vec![Lang::parse("nl").unwrap(), Lang::parse("en").unwrap()], prefer_sdh: false };
        let mut c = vec![
            cand("en-hash", "en", "whatever", true),
            cand("nl-bluray", "nl", "Show.S01E01.720p.BluRay.x264-DEMAND", false),
            cand("nl-web", "nl", "Show.S01E01.1080p.WEB-DL.H.264-NTb", false),
            cand("nl-hash", "nl", "x", true),
        ];
        rank(&mut c, &q, &prefs);
        let ids: Vec<_> = c.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["nl-hash", "nl-web", "nl-bluray", "en-hash"]);
    }
}
