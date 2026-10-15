//! Typed client for the parts of the Jellyfin 10.10+ REST API redjelly uses
//! (SPEC §3).

mod client;
pub mod models;

pub use client::{Client, ClientInfo, Error};
