//! Normalises downloaded subtitle bytes to UTF-8 text. Dutch subtitles are
//! frequently Windows-1252, older ones sometimes UTF-16 with a BOM.

use encoding_rs::Encoding;

pub fn to_utf8(bytes: &[u8]) -> String {
    let text = match Encoding::for_bom(bytes) {
        Some((enc, bom_len)) => enc.decode_without_bom_handling(&bytes[bom_len..]).0,
        None => {
            let mut det = chardetng::EncodingDetector::new();
            det.feed(bytes, true);
            det.guess(None, true).decode_without_bom_handling(bytes).0
        }
    };
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Guesses the format from content, ignoring whatever the provider claimed.
pub fn detect_format(text: &str) -> Option<&'static str> {
    let head = text.trim_start();
    if head.starts_with("WEBVTT") {
        Some("vtt")
    } else if head.starts_with("[Script Info]") {
        Some("ass")
    } else if head.lines().take(3).any(|l| l.contains(" --> ")) {
        Some("srt")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_windows_1252() {
        // "één ç" in Windows-1252, long enough for detection to be confident.
        let mut bytes = b"1\r\n00:00:01,000 --> 00:00:02,000\r\n".to_vec();
        for _ in 0..20 {
            bytes.extend_from_slice(b"Hij zei \xe9\xe9n keer: \"ga n\xe1 de caf\xe9\".\r\n");
        }
        let text = to_utf8(&bytes);
        assert!(text.contains("één keer"), "{text}");
        assert!(!text.contains('\r'));
    }

    #[test]
    fn strips_utf8_bom() {
        assert_eq!(to_utf8(b"\xef\xbb\xbfWEBVTT"), "WEBVTT");
    }

    #[test]
    fn detects_formats() {
        assert_eq!(detect_format("1\n00:00:01,000 --> 00:00:02,000\nhi"), Some("srt"));
        assert_eq!(detect_format("WEBVTT\n\n"), Some("vtt"));
        assert_eq!(detect_format("[Script Info]\nTitle: x"), Some("ass"));
        assert_eq!(detect_format("<html>"), None);
    }
}
