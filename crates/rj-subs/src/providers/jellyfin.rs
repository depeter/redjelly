//! The Jellyfin server's own remote subtitle search. The server downloads
//! and stores the file itself, so `download` returns `None`.

use rj_core::lang::Lang;

use crate::provider::{Candidate, Error, Provider, ProviderId, Query, SubtitleFile};

pub struct JellyfinServer {
    client: rj_jellyfin::Client,
}

impl JellyfinServer {
    pub fn new(client: rj_jellyfin::Client) -> Self {
        Self { client }
    }
}

fn map_err(e: rj_jellyfin::Error) -> Error {
    Error::Protocol("jellyfin", e.to_string())
}

#[async_trait::async_trait]
impl Provider for JellyfinServer {
    fn id(&self) -> ProviderId {
        ProviderId::JellyfinServer
    }

    async fn search(&self, q: &Query) -> Result<Vec<Candidate>, Error> {
        let mut out = Vec::new();
        for lang in &q.languages {
            let results = self
                .client
                .remote_subtitle_search(&q.jellyfin_item_id, lang.iso639_2b())
                .await
                .map_err(map_err)?;
            out.extend(results.into_iter().map(|r| Candidate {
                provider: ProviderId::JellyfinServer,
                lang: r
                    .three_letter_iso_language_name
                    .as_deref()
                    .and_then(Lang::parse)
                    .unwrap_or_else(|| lang.clone()),
                release: r.name.unwrap_or_default(),
                format: r.format,
                hash_match: r.is_hash_match.unwrap_or(false),
                hearing_impaired: r.hearing_impaired.unwrap_or(false),
                forced: false,
                downloads: r.download_count.unwrap_or(0),
                rating: None,
                id: r.id,
            }));
        }
        Ok(out)
    }

    async fn download(&self, q: &Query, c: &Candidate) -> Result<Option<SubtitleFile>, Error> {
        self.client
            .remote_subtitle_download(&q.jellyfin_item_id, &c.id)
            .await
            .map_err(map_err)?;
        Ok(None)
    }
}
