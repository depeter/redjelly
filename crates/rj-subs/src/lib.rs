//! Subtitle search, download, clean-up and ranking (SPEC §6).

pub mod encoding;
pub mod hash;
pub mod provider;
pub mod providers;
pub mod rank;
pub mod sync;
pub mod timing;

pub use provider::{Candidate, Provider, ProviderId, Query, SubtitleFile};
