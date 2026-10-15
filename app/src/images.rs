//! Poster/backdrop loading: disk cache (keyed by Jellyfin image tag, so
//! entries never go stale), bounded concurrent downloads, off-thread decode.

use std::path::PathBuf;
use std::sync::Arc;

use slint::{Rgb8Pixel, SharedPixelBuffer};
use tokio::sync::Semaphore;

/// Where to fetch an image and its cache key.
#[derive(Debug, Clone)]
pub struct ImageSrc {
    pub url: String,
    pub key: String,
}

#[derive(Clone)]
pub struct ImageLoader {
    http: reqwest::Client,
    dir: PathBuf,
    permits: Arc<Semaphore>,
}

impl ImageLoader {
    pub fn new(cache_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&cache_dir);
        Self { http: reqwest::Client::new(), dir: cache_dir, permits: Arc::new(Semaphore::new(6)) }
    }

    pub async fn load(&self, src: &ImageSrc) -> anyhow::Result<SharedPixelBuffer<Rgb8Pixel>> {
        let file: String = src.key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        let path = self.dir.join(format!("{file}.jpg"));
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(_) => {
                let _permit = self.permits.acquire().await?;
                let b = self.http.get(&src.url).send().await?.error_for_status()?.bytes().await?.to_vec();
                if let Err(e) = tokio::fs::write(&path, &b).await {
                    tracing::debug!("image cache write {}: {e}", path.display());
                }
                b
            }
        };
        tokio::task::spawn_blocking(move || {
            let img = image::load_from_memory(&bytes)?.to_rgb8();
            Ok(SharedPixelBuffer::clone_from_slice(img.as_raw(), img.width(), img.height()))
        })
        .await?
    }
}
