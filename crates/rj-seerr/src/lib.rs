//! Typed client for the parts of the Jellyseerr / Overseerr API redjelly's
//! Discover screens use: TMDb discovery, title details, RT/IMDb ratings and
//! requests.

pub mod arr;
mod client;
pub mod models;

pub use arr::{ArrClient, ArrError, SearchScope};
pub use client::{Client, DiscoverQuery, Error, Sort};
pub use models::MediaKind;
