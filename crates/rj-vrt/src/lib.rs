//! VRT MAX: the public broadcaster's catalogue (GraphQL), TV guide and the
//! unencrypted live streams.
//!
//! Most VRT video is DRM protected, which mpv can't play. redjelly shows the
//! catalogue natively and plays those items in VRT's own web player (Windows,
//! WebView2); this crate only hands out the page URL for them. VRT NWS and
//! Ketnet Junior are clear HLS and resolve to a stream URL here
//! ([`stream::live_url`]).

pub mod channels;
mod client;
pub mod epg;
pub mod models;
pub mod stream;

pub use channels::{CHANNELS, Channel};
pub use client::{Client, Error, PAGE_SIZE, SITE, token_expiry};
pub use epg::{Guide, Programme};
pub use models::{Page, Section, Tile, TileAction, TileKind};
