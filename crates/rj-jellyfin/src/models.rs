//! Subsets of Jellyfin DTOs. Unknown fields are ignored; everything optional
//! on the server side is `Option` or defaulted here.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Jellyfin time unit: 100 ns.
pub const TICKS_PER_SECOND: i64 = 10_000_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct QueryResult<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    #[serde(default)]
    pub total_record_count: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct User {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub primary_image_tag: Option<String>,
    #[serde(default)]
    pub has_password: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AuthenticationResult {
    pub user: User,
    pub access_token: String,
    pub server_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Item {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "Type", default)]
    pub kind: String,
    #[serde(default)]
    pub series_id: Option<String>,
    #[serde(default)]
    pub series_name: Option<String>,
    #[serde(default)]
    pub season_id: Option<String>,
    #[serde(default)]
    pub index_number: Option<u32>,
    #[serde(default)]
    pub parent_index_number: Option<u32>,
    #[serde(default)]
    pub production_year: Option<u32>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub run_time_ticks: Option<i64>,
    #[serde(default)]
    pub provider_ids: HashMap<String, String>,
    #[serde(default)]
    pub image_tags: HashMap<String, String>,
    #[serde(default)]
    pub user_data: Option<UserData>,
    #[serde(default)]
    pub media_sources: Vec<MediaSource>,
    #[serde(default)]
    pub collection_type: Option<String>,
    #[serde(default)]
    pub is_folder: bool,
    #[serde(default)]
    pub backdrop_image_tags: Vec<String>,
    #[serde(default)]
    pub parent_backdrop_item_id: Option<String>,
    #[serde(default)]
    pub parent_backdrop_image_tags: Vec<String>,
    #[serde(default)]
    pub parent_thumb_item_id: Option<String>,
    #[serde(default)]
    pub parent_thumb_image_tag: Option<String>,
    #[serde(default)]
    pub series_primary_image_tag: Option<String>,
    #[serde(default)]
    pub community_rating: Option<f32>,
    #[serde(default)]
    pub official_rating: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub child_count: Option<u32>,
    #[serde(default)]
    pub season_name: Option<String>,
}

/// Which image to show for an item, resolved from the fields above.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub item_id: String,
    /// `Primary`, `Thumb` or `Backdrop`.
    pub kind: &'static str,
    pub tag: String,
}

impl Item {
    /// Portrait poster (movies, series, seasons); episodes fall back to the
    /// series poster.
    pub fn poster(&self) -> Option<ImageRef> {
        if let Some(tag) = self.image_tags.get("Primary").filter(|_| self.kind != "Episode") {
            return Some(ImageRef { item_id: self.id.clone(), kind: "Primary", tag: tag.clone() });
        }
        let series = self.series_id.as_ref()?;
        let tag = self.series_primary_image_tag.as_ref()?;
        Some(ImageRef { item_id: series.clone(), kind: "Primary", tag: tag.clone() })
    }

    /// 16:9 still: episode screenshot, else thumb, else backdrop.
    pub fn landscape(&self) -> Option<ImageRef> {
        let own = |kind: &'static str| {
            self.image_tags.get(kind).map(|t| ImageRef { item_id: self.id.clone(), kind, tag: t.clone() })
        };
        if self.kind == "Episode"
            && let Some(r) = own("Primary") {
                return Some(r);
            }
        own("Thumb")
            .or_else(|| {
                let tag = self.backdrop_image_tags.first()?;
                Some(ImageRef { item_id: self.id.clone(), kind: "Backdrop", tag: tag.clone() })
            })
            .or_else(|| {
                Some(ImageRef {
                    item_id: self.parent_thumb_item_id.clone()?,
                    kind: "Thumb",
                    tag: self.parent_thumb_image_tag.clone()?,
                })
            })
            .or_else(|| {
                Some(ImageRef {
                    item_id: self.parent_backdrop_item_id.clone()?,
                    kind: "Backdrop",
                    tag: self.parent_backdrop_image_tags.first()?.clone(),
                })
            })
            .or_else(|| own("Primary"))
    }

    pub fn backdrop(&self) -> Option<ImageRef> {
        if let Some(tag) = self.backdrop_image_tags.first() {
            return Some(ImageRef { item_id: self.id.clone(), kind: "Backdrop", tag: tag.clone() });
        }
        Some(ImageRef {
            item_id: self.parent_backdrop_item_id.clone()?,
            kind: "Backdrop",
            tag: self.parent_backdrop_image_tags.first()?.clone(),
        })
    }

    /// Resume position as a fraction, if partially watched.
    pub fn progress(&self) -> Option<f64> {
        let ud = self.user_data.as_ref()?;
        let total = self.run_time_ticks.filter(|t| *t > 0)?;
        (ud.playback_position_ticks > 0).then(|| ud.playback_position_ticks as f64 / total as f64)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct QuickConnect {
    pub secret: String,
    pub code: String,
    #[serde(default)]
    pub authenticated: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserData {
    #[serde(default)]
    pub playback_position_ticks: i64,
    #[serde(default)]
    pub played_percentage: Option<f64>,
    #[serde(default)]
    pub played: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaSource {
    pub id: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub run_time_ticks: Option<i64>,
    #[serde(default)]
    pub supports_direct_play: bool,
    #[serde(default)]
    pub transcoding_url: Option<String>,
    #[serde(default)]
    pub media_streams: Vec<MediaStream>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum StreamType {
    Audio,
    Video,
    Subtitle,
    EmbeddedImage,
    Data,
    Lyric,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaStream {
    pub index: u32,
    #[serde(rename = "Type")]
    pub kind: StreamType,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub display_title: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default)]
    pub is_forced: bool,
    #[serde(default)]
    pub is_hearing_impaired: bool,
    #[serde(default)]
    pub is_external: bool,
    #[serde(default)]
    pub is_text_subtitle_stream: bool,
    #[serde(default)]
    pub channels: Option<u32>,
    #[serde(default)]
    pub delivery_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackInfo {
    #[serde(default)]
    pub media_sources: Vec<MediaSource>,
    #[serde(default)]
    pub play_session_id: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum SegmentType {
    Unknown,
    Commercial,
    Preview,
    Recap,
    Outro,
    Intro,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaSegment {
    #[serde(rename = "Type")]
    pub kind: SegmentType,
    pub start_ticks: i64,
    pub end_ticks: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct RemoteSubtitle {
    pub id: String,
    #[serde(default)]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub three_letter_iso_language_name: Option<String>,
    #[serde(default)]
    pub download_count: Option<u32>,
    #[serde(default)]
    pub is_hash_match: Option<bool>,
    #[serde(default)]
    pub hearing_impaired: Option<bool>,
}

/// Body for `POST /Videos/{id}/Subtitles`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct UploadSubtitle {
    /// ISO 639-2/B, e.g. `dut`.
    pub language: String,
    /// File extension: `srt`, `ass`, `vtt`.
    pub format: String,
    pub is_forced: bool,
    pub is_hearing_impaired: bool,
    /// Base64 of the UTF-8 file contents.
    pub data: String,
}

/// Playback start/progress/stop report.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackReport {
    pub item_id: String,
    pub media_source_id: String,
    pub play_session_id: Option<String>,
    pub position_ticks: i64,
    pub is_paused: bool,
    pub is_muted: bool,
    pub volume_level: u32,
    pub audio_stream_index: Option<u32>,
    pub subtitle_stream_index: Option<i32>,
    pub play_method: &'static str,
    pub can_seek: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_images_fall_back_to_series() {
        let json = r#"{"Id": "ep", "Type": "Episode", "SeriesId": "show",
            "SeriesPrimaryImageTag": "sp", "ImageTags": {"Primary": "still"},
            "ParentBackdropItemId": "show", "ParentBackdropImageTags": ["bd"]}"#;
        let item: Item = serde_json::from_str(json).unwrap();
        assert_eq!(item.poster().unwrap(), ImageRef { item_id: "show".into(), kind: "Primary", tag: "sp".into() });
        assert_eq!(item.landscape().unwrap().tag, "still");
        assert_eq!(item.backdrop().unwrap().tag, "bd");
    }

    #[test]
    fn parses_item_with_streams() {
        let json = r#"{
            "Id": "abc", "Name": "Pilot", "Type": "Episode", "SeriesName": "Show",
            "IndexNumber": 1, "ParentIndexNumber": 1, "ProviderIds": {"Imdb": "tt1"},
            "MediaSources": [{"Id": "ms", "Path": "/x.mkv", "SupportsDirectPlay": true,
              "MediaStreams": [
                {"Index": 0, "Type": "Video", "Codec": "hevc"},
                {"Index": 1, "Type": "Audio", "Language": "eng", "Channels": 6, "IsDefault": true},
                {"Index": 2, "Type": "Subtitle", "Language": "dut", "Codec": "subrip",
                 "IsTextSubtitleStream": true, "SomethingNew": 1}
              ]}]
        }"#;
        let item: Item = serde_json::from_str(json).unwrap();
        assert_eq!(item.provider_ids["Imdb"], "tt1");
        let streams = &item.media_sources[0].media_streams;
        assert_eq!(streams[2].kind, StreamType::Subtitle);
        assert_eq!(streams[1].channels, Some(6));
    }
}
