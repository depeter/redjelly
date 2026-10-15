//! OpenSubtitles "moviehash": file size plus the wrapping sum of the
//! little-endian u64 words in the first and last 64 KiB.

use reqwest::header::RANGE;

pub const CHUNK: u64 = 64 * 1024;

/// Hash from the two 64 KiB chunks. Files smaller than one chunk can't be
/// hashed (matches the reference implementation).
pub fn movie_hash(size: u64, head: &[u8], tail: &[u8]) -> Option<String> {
    if size < CHUNK || head.len() as u64 != CHUNK || tail.len() as u64 != CHUNK {
        return None;
    }
    let sum = |buf: &[u8]| {
        buf.chunks_exact(8)
            .map(|w| u64::from_le_bytes(w.try_into().unwrap()))
            .fold(0u64, u64::wrapping_add)
    };
    let h = size.wrapping_add(sum(head)).wrapping_add(sum(tail));
    Some(format!("{h:016x}"))
}

/// Hashes a remote file with two Range requests instead of downloading it.
pub async fn remote_movie_hash(
    http: &reqwest::Client,
    url: &str,
    auth_header: &str,
    size: u64,
) -> Result<Option<String>, reqwest::Error> {
    if size < CHUNK {
        return Ok(None);
    }
    let fetch = |from: u64| async move {
        http.get(url)
            .header("Authorization", auth_header)
            .header(RANGE, format!("bytes={}-{}", from, from + CHUNK - 1))
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await
    };
    let head = fetch(0).await?;
    let tail = fetch(size - CHUNK).await?;
    Ok(movie_hash(size, &head, &tail))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_known_pattern() {
        let head = vec![1u8; CHUNK as usize];
        let tail = vec![0u8; CHUNK as usize];
        // Each head word is 0x0101010101010101, 8192 of them.
        let expected = (CHUNK * 2).wrapping_add(0x0101_0101_0101_0101u64.wrapping_mul(8192));
        assert_eq!(movie_hash(CHUNK * 2, &head, &tail), Some(format!("{expected:016x}")));
    }

    #[test]
    fn too_small_is_none() {
        assert_eq!(movie_hash(10, &[], &[]), None);
    }
}
