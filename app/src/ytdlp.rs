//! yt-dlp, which mpv runs to play web URLs (YouTube trailers). Downloaded
//! from its GitHub releases on first use and refreshed weekly, since sites
//! keep breaking old versions.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);
const RELEASES: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";

/// (file name on disk, release asset)
const fn names() -> (&'static str, &'static str) {
    if cfg!(windows) {
        ("yt-dlp.exe", "yt-dlp.exe")
    } else if cfg!(target_os = "macos") {
        ("yt-dlp", "yt-dlp_macos")
    } else {
        ("yt-dlp", "yt-dlp_linux")
    }
}

pub fn path(tools_dir: &Path) -> PathBuf {
    tools_dir.join(names().0)
}

fn fresh(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| SystemTime::now().duration_since(t).unwrap_or_default() < MAX_AGE)
}

/// Makes sure yt-dlp is present and recent. A failed refresh keeps the
/// old copy; only a missing one is an error.
pub async fn ensure(http: &reqwest::Client, path: &Path) -> anyhow::Result<()> {
    if fresh(path) {
        return Ok(());
    }
    match download(http, path).await {
        Ok(()) => Ok(()),
        Err(e) if path.exists() => {
            tracing::warn!("refreshing yt-dlp failed, keeping the old one: {e:#}");
            Ok(())
        }
        Err(e) => Err(e.context("downloading yt-dlp")),
    }
}

async fn download(http: &reqwest::Client, path: &Path) -> anyhow::Result<()> {
    let url = format!("{RELEASES}/{}", names().1);
    tracing::info!("downloading {url}");
    let bytes = http.get(&url).send().await?.error_for_status()?.bytes().await?;
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let part = path.with_extension("part");
    tokio::fs::write(&part, &bytes).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755)).await?;
    }
    tokio::fs::rename(&part, path).await?;
    Ok(())
}
