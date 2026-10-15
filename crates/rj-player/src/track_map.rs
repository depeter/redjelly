//! Maps Jellyfin `MediaStream.Index` values to mpv track ids (SPEC §3).
//!
//! mpv numbers embedded tracks per type, 1-based, in container order.
//! Jellyfin indexes all streams together, with external subtitles after the
//! embedded ones. External subtitles get their mpv id when `sub-add` runs.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Video,
    Audio,
    Sub,
}

#[derive(Debug, Clone, Copy)]
pub struct StreamInfo {
    pub jf_index: u32,
    pub kind: Kind,
    pub external: bool,
}

#[derive(Debug, Default, Clone)]
pub struct TrackMap {
    to_mpv: HashMap<u32, (Kind, i64)>,
}

impl TrackMap {
    pub fn new(streams: &[StreamInfo]) -> Self {
        let mut sorted: Vec<_> = streams.iter().filter(|s| !s.external).collect();
        sorted.sort_by_key(|s| s.jf_index);
        let mut counters: HashMap<Kind, i64> = HashMap::new();
        let mut to_mpv = HashMap::new();
        for s in sorted {
            let n = counters.entry(s.kind).or_insert(0);
            *n += 1;
            to_mpv.insert(s.jf_index, (s.kind, *n));
        }
        Self { to_mpv }
    }

    /// Records the id mpv assigned to an external subtitle after `sub-add`.
    pub fn register_external(&mut self, jf_index: u32, mpv_id: i64) {
        self.to_mpv.insert(jf_index, (Kind::Sub, mpv_id));
    }

    pub fn mpv_id(&self, jf_index: u32) -> Option<(Kind, i64)> {
        self.to_mpv.get(&jf_index).copied()
    }

    pub fn jf_index(&self, kind: Kind, mpv_id: i64) -> Option<u32> {
        self.to_mpv
            .iter()
            .find(|(_, v)| **v == (kind, mpv_id))
            .map(|(k, _)| *k)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(jf_index: u32, kind: Kind, external: bool) -> StreamInfo {
        StreamInfo { jf_index, kind, external }
    }

    #[test]
    fn numbers_per_type_in_container_order() {
        let mut m = TrackMap::new(&[
            s(0, Kind::Video, false),
            s(1, Kind::Audio, false),
            s(2, Kind::Audio, false),
            s(3, Kind::Sub, false),
            s(4, Kind::Sub, false),
            s(5, Kind::Sub, true),
        ]);
        assert_eq!(m.mpv_id(2), Some((Kind::Audio, 2)));
        assert_eq!(m.mpv_id(4), Some((Kind::Sub, 2)));
        assert_eq!(m.mpv_id(5), None);
        m.register_external(5, 3);
        assert_eq!(m.mpv_id(5), Some((Kind::Sub, 3)));
        assert_eq!(m.jf_index(Kind::Sub, 3), Some(5));
    }
}
