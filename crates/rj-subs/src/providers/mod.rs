//! Provider implementations. Each is independent; failures in one never
//! block results from the others.

pub mod jellyfin;
pub mod opensubtitles;
pub mod subdl;
