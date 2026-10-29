use base64::Engine as _;
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::json;
use url::Url;

use crate::models::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid server URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("not signed in")]
    NoSession,
    #[error("wrong username or password")]
    BadCredentials,
    #[error("server returned {status} for {path}")]
    Status { status: StatusCode, path: String },
}

/// Identifies this app to the server (shown in the Jellyfin dashboard).
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub client: String,
    pub device: String,
    pub device_id: String,
    pub version: String,
}

#[derive(Debug, Clone)]
struct Session {
    user_id: String,
    token: String,
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    info: ClientInfo,
    session: Option<Session>,
}

impl Client {
    pub fn new(server_url: &str, info: ClientInfo) -> Result<Self, Error> {
        let mut base = Url::parse(server_url)?;
        // Ensure joins append rather than replace the last path segment.
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        // Without timeouts a request on a dead connection (server or Wi-Fi
        // blip) hangs forever, and the "busy" guard then blocks every later
        // play until restart. Read timeout = silence, not total length.
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .read_timeout(std::time::Duration::from_secs(30))
            .build()?;
        Ok(Self { http, base, info, session: None })
    }

    /// Restores a session from a stored token.
    pub fn with_session(mut self, user_id: String, token: String) -> Self {
        self.session = Some(Session { user_id, token });
        self
    }

    pub fn user_id(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.user_id.as_str())
    }

    pub fn token(&self) -> Option<&str> {
        self.session.as_ref().map(|s| s.token.as_str())
    }

    /// The `Authorization` header value; also passed to mpv for streams.
    pub fn auth_header(&self) -> String {
        let i = &self.info;
        let mut h = format!(
            r#"MediaBrowser Client="{}", Device="{}", DeviceId="{}", Version="{}""#,
            i.client, i.device, i.device_id, i.version
        );
        if let Some(s) = &self.session {
            h.push_str(&format!(r#", Token="{}""#, s.token));
        }
        h
    }

    pub fn url(&self, path: &str) -> Result<Url, Error> {
        Ok(self.base.join(path.trim_start_matches('/'))?)
    }

    fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, Error> {
        Ok(self
            .http
            .request(method, self.url(path)?)
            .header("Authorization", self.auth_header()))
    }

    fn session(&self) -> Result<&Session, Error> {
        self.session.as_ref().ok_or(Error::NoSession)
    }

    async fn send(rb: RequestBuilder, path: &str) -> Result<reqwest::Response, Error> {
        let resp = rb.send().await?;
        let status = resp.status();
        if status.is_success() {
            Ok(resp)
        } else {
            Err(Error::Status { status, path: path.into() })
        }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T, Error> {
        let rb = self.request(Method::GET, path)?.query(query);
        Ok(Self::send(rb, path).await?.json().await?)
    }

    async fn post_json(&self, path: &str, body: &impl serde::Serialize) -> Result<reqwest::Response, Error> {
        let rb = self.request(Method::POST, path)?.json(body);
        Self::send(rb, path).await
    }

    // ---- Auth --------------------------------------------------------------

    pub async fn public_users(&self) -> Result<Vec<User>, Error> {
        self.get("Users/Public", &[]).await
    }

    pub async fn authenticate(&mut self, username: &str, password: &str) -> Result<AuthenticationResult, Error> {
        let path = "Users/AuthenticateByName";
        let body = json!({ "Username": username, "Pw": password });
        let resp = match self.post_json(path, &body).await {
            Err(Error::Status { status: StatusCode::UNAUTHORIZED, .. }) => return Err(Error::BadCredentials),
            r => r?,
        };
        let auth: AuthenticationResult = resp.json().await?;
        self.session = Some(Session { user_id: auth.user.id.clone(), token: auth.access_token.clone() });
        Ok(auth)
    }

    /// Checks a restored token is still valid.
    pub async fn me(&self) -> Result<User, Error> {
        match self.get("Users/Me", &[]).await {
            Err(Error::Status { status: StatusCode::UNAUTHORIZED, .. }) => Err(Error::BadCredentials),
            r => r,
        }
    }

    pub async fn quick_connect_enabled(&self) -> Result<bool, Error> {
        self.get("QuickConnect/Enabled", &[]).await
    }

    /// Starts Quick Connect; show `code` to the user, then poll
    /// [`Self::quick_connect_state`] with `secret`.
    pub async fn quick_connect_initiate(&self) -> Result<QuickConnect, Error> {
        let path = "QuickConnect/Initiate";
        Ok(Self::send(self.request(Method::POST, path)?, path).await?.json().await?)
    }

    pub async fn quick_connect_state(&self, secret: &str) -> Result<QuickConnect, Error> {
        self.get("QuickConnect/Connect", &[("secret", secret)]).await
    }

    pub async fn authenticate_quick_connect(&mut self, secret: &str) -> Result<AuthenticationResult, Error> {
        let resp = self
            .post_json("Users/AuthenticateWithQuickConnect", &json!({ "Secret": secret }))
            .await?;
        let auth: AuthenticationResult = resp.json().await?;
        self.session = Some(Session { user_id: auth.user.id.clone(), token: auth.access_token.clone() });
        Ok(auth)
    }

    // ---- Browse ------------------------------------------------------------

    pub async fn user_views(&self) -> Result<QueryResult<Item>, Error> {
        let s = self.session()?;
        self.get("UserViews", &[("userId", &s.user_id)]).await
    }

    pub async fn resume(&self, limit: u32) -> Result<QueryResult<Item>, Error> {
        let s = self.session()?;
        let limit = limit.to_string();
        self.get(
            "UserItems/Resume",
            &[("userId", &s.user_id), ("limit", &limit), ("mediaTypes", "Video")],
        )
        .await
    }

    pub async fn next_up(&self, limit: u32) -> Result<QueryResult<Item>, Error> {
        let s = self.session()?;
        let limit = limit.to_string();
        self.get("Shows/NextUp", &[("userId", &s.user_id), ("limit", &limit)]).await
    }

    pub async fn latest(&self, parent_id: &str, limit: u32) -> Result<Vec<Item>, Error> {
        let s = self.session()?;
        let limit = limit.to_string();
        self.get("Items/Latest", &[("userId", &s.user_id), ("parentId", parent_id), ("limit", &limit)])
            .await
    }

    /// Generic `/Items` query; `query` holds filters like `parentId`,
    /// `searchTerm`, `sortBy`, `startIndex`, `limit`.
    pub async fn items(&self, query: &[(&str, &str)]) -> Result<QueryResult<Item>, Error> {
        let s = self.session()?;
        let mut q = vec![
            ("userId", s.user_id.as_str()),
            ("fields", "Overview,Genres,ChildCount,PrimaryImageAspectRatio"),
            ("enableImageTypes", "Primary,Backdrop,Thumb"),
        ];
        q.extend_from_slice(query);
        self.get("Items", &q).await
    }

    /// All episodes of a series, in order, with user data.
    pub async fn episodes(&self, series_id: &str) -> Result<QueryResult<Item>, Error> {
        let s = self.session()?;
        self.get(
            &format!("Shows/{series_id}/Episodes"),
            &[("userId", &s.user_id), ("fields", "Overview")],
        )
        .await
    }

    /// Episodes including media streams (to see which lack subtitles).
    pub async fn episodes_with_media(&self, series_id: &str) -> Result<QueryResult<Item>, Error> {
        let s = self.session()?;
        self.get(
            &format!("Shows/{series_id}/Episodes"),
            &[("userId", &s.user_id), ("fields", "MediaSources")],
        )
        .await
    }

    /// The episodes before and after `item_id` (plus itself), in order.
    pub async fn adjacent_episodes(&self, series_id: &str, item_id: &str) -> Result<QueryResult<Item>, Error> {
        let s = self.session()?;
        self.get(&format!("Shows/{series_id}/Episodes"), &[("userId", &s.user_id), ("adjacentTo", item_id)]).await
    }

    pub async fn next_up_for_series(&self, series_id: &str) -> Result<QueryResult<Item>, Error> {
        let s = self.session()?;
        self.get("Shows/NextUp", &[("userId", &s.user_id), ("seriesId", series_id), ("limit", "1")])
            .await
    }

    pub async fn search(&self, term: &str, limit: u32) -> Result<QueryResult<Item>, Error> {
        let limit = limit.to_string();
        self.items(&[
            ("searchTerm", term),
            ("recursive", "true"),
            ("includeItemTypes", "Movie,Series,Episode"),
            ("limit", &limit),
        ])
        .await
    }

    pub async fn set_played(&self, item_id: &str, played: bool) -> Result<(), Error> {
        let s = self.session()?;
        let path = format!("UserPlayedItems/{item_id}");
        let method = if played { Method::POST } else { Method::DELETE };
        let rb = self.request(method, &path)?.query(&[("userId", &s.user_id)]);
        Self::send(rb, &path).await.map(drop)
    }

    pub async fn item(&self, id: &str) -> Result<Item, Error> {
        let s = self.session()?;
        self.get(&format!("Items/{id}"), &[("userId", &s.user_id)]).await
    }

    /// Trailer files stored next to the item on the server.
    pub async fn local_trailers(&self, id: &str) -> Result<Vec<Item>, Error> {
        let s = self.session()?;
        self.get(&format!("Items/{id}/LocalTrailers"), &[("userId", &s.user_id)]).await
    }

    pub fn image_url(&self, item_id: &str, kind: &str, tag: &str, max_width: u32) -> Result<Url, Error> {
        let mut u = self.url(&format!("Items/{item_id}/Images/{kind}"))?;
        u.query_pairs_mut()
            .append_pair("tag", tag)
            .append_pair("maxWidth", &max_width.to_string())
            .append_pair("format", "Jpg")
            .append_pair("quality", "90");
        Ok(u)
    }

    // ---- Playback ----------------------------------------------------------

    /// Asks for playback info with a profile that claims every container and
    /// codec is direct-playable — mpv decodes everything (SPEC §3).
    pub async fn playback_info(&self, item_id: &str, start_ticks: i64) -> Result<PlaybackInfo, Error> {
        let s = self.session()?;
        let body = json!({
            "UserId": s.user_id,
            "StartTimeTicks": start_ticks,
            "MaxStreamingBitrate": 1_000_000_000u64,
            "EnableDirectPlay": true,
            "EnableDirectStream": true,
            "EnableTranscoding": true,
            "AutoOpenLiveStream": true,
            "DeviceProfile": {
                "Name": "redjelly (mpv)",
                "MaxStreamingBitrate": 1_000_000_000u64,
                "DirectPlayProfiles": [{ "Type": "Video" }, { "Type": "Audio" }],
                "TranscodingProfiles": [{
                    "Type": "Video", "Container": "ts", "Protocol": "hls",
                    "VideoCodec": "h264,hevc", "AudioCodec": "aac,ac3,eac3,opus"
                }],
                "SubtitleProfiles": [
                    { "Format": "srt", "Method": "External" },
                    { "Format": "ass", "Method": "External" },
                    { "Format": "ssa", "Method": "External" },
                    { "Format": "vtt", "Method": "External" },
                    { "Format": "srt", "Method": "Embed" },
                    { "Format": "ass", "Method": "Embed" },
                    { "Format": "pgssub", "Method": "Embed" },
                    { "Format": "dvdsub", "Method": "Embed" },
                    { "Format": "dvbsub", "Method": "Embed" }
                ]
            }
        });
        let path = format!("Items/{item_id}/PlaybackInfo");
        Ok(self.post_json(&path, &body).await?.json().await?)
    }

    pub fn direct_stream_url(&self, item_id: &str, media_source_id: &str) -> Result<Url, Error> {
        let mut u = self.url(&format!("Videos/{item_id}/stream"))?;
        u.query_pairs_mut()
            .append_pair("static", "true")
            .append_pair("mediaSourceId", media_source_id);
        Ok(u)
    }

    /// Audio-only server transcode (16 kHz mono MP3) of one audio stream,
    /// so AI subtitles don't pull the whole video over the network.
    pub fn audio_only_url(&self, item_id: &str, media_source_id: &str, audio_index: Option<u32>) -> Result<Url, Error> {
        let mut u = self.url(&format!("Audio/{item_id}/stream.mp3"))?;
        {
            let mut q = u.query_pairs_mut();
            q.append_pair("static", "false")
                .append_pair("mediaSourceId", media_source_id)
                .append_pair("audioCodec", "mp3")
                .append_pair("audioBitRate", "64000")
                .append_pair("audioChannels", "1")
                .append_pair("audioSampleRate", "16000")
                // Its own device id, so stopping playback on this device
                // never kills the extraction's transcode.
                .append_pair("deviceId", &format!("{}-ai", self.info.device_id));
            if let Some(i) = audio_index {
                q.append_pair("audioStreamIndex", &i.to_string());
            }
        }
        Ok(u)
    }

    pub fn subtitle_url(&self, item_id: &str, media_source_id: &str, index: u32, format: &str) -> Result<Url, Error> {
        self.url(&format!("Videos/{item_id}/{media_source_id}/Subtitles/{index}/Stream.{format}"))
    }

    pub async fn report_start(&self, r: &PlaybackReport) -> Result<(), Error> {
        self.post_json("Sessions/Playing", r).await.map(drop)
    }

    pub async fn report_progress(&self, r: &PlaybackReport) -> Result<(), Error> {
        self.post_json("Sessions/Playing/Progress", r).await.map(drop)
    }

    pub async fn report_stopped(&self, r: &PlaybackReport) -> Result<(), Error> {
        self.post_json("Sessions/Playing/Stopped", r).await.map(drop)
    }

    pub async fn media_segments(&self, item_id: &str) -> Result<QueryResult<MediaSegment>, Error> {
        self.get(&format!("MediaSegments/{item_id}"), &[]).await
    }

    // ---- Subtitles ---------------------------------------------------------

    /// Server-side provider search (uses the server's subtitle plugins).
    /// `lang` is ISO 639-2/B.
    pub async fn remote_subtitle_search(&self, item_id: &str, lang: &str) -> Result<Vec<RemoteSubtitle>, Error> {
        self.get(&format!("Items/{item_id}/RemoteSearch/Subtitles/{lang}"), &[]).await
    }

    /// Makes the server download and store a remote subtitle itself.
    pub async fn remote_subtitle_download(&self, item_id: &str, subtitle_id: &str) -> Result<(), Error> {
        let path = format!("Items/{item_id}/RemoteSearch/Subtitles/{subtitle_id}");
        Self::send(self.request(Method::POST, &path)?, &path).await.map(drop)
    }

    /// Uploads a UTF-8 subtitle file to be stored next to the media.
    pub async fn upload_subtitle(
        &self,
        item_id: &str,
        lang_639_2b: &str,
        format: &str,
        contents: &[u8],
        forced: bool,
        hearing_impaired: bool,
    ) -> Result<(), Error> {
        let body = UploadSubtitle {
            language: lang_639_2b.into(),
            format: format.into(),
            is_forced: forced,
            is_hearing_impaired: hearing_impaired,
            data: base64::engine::general_purpose::STANDARD.encode(contents),
        };
        self.post_json(&format!("Videos/{item_id}/Subtitles"), &body).await.map(drop)
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_partial_json, header_regex, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn info() -> ClientInfo {
        ClientInfo { client: "redjelly".into(), device: "test".into(), device_id: "dev1".into(), version: "0.1.0".into() }
    }

    #[tokio::test]
    async fn authenticates_and_sends_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/jf/Users/AuthenticateByName"))
            .and(body_partial_json(json!({ "Username": "peter" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "User": { "Id": "u1", "Name": "peter" },
                "AccessToken": "tok", "ServerId": "s1"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/jf/UserViews"))
            .and(header_regex("Authorization", r#"Token="tok""#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "Items": [], "TotalRecordCount": 0 })))
            .mount(&server)
            .await;

        // A base path without trailing slash must still work (reverse proxies).
        let mut c = Client::new(&format!("{}/jf", server.uri()), info()).unwrap();
        c.authenticate("peter", "pw").await.unwrap();
        assert_eq!(c.user_id(), Some("u1"));
        assert_eq!(c.user_views().await.unwrap().total_record_count, 0);
    }

    #[tokio::test]
    async fn quick_connect_flow() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/QuickConnect/Initiate"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "Secret": "sec", "Code": "123456", "Authenticated": false
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/Users/AuthenticateWithQuickConnect"))
            .and(body_partial_json(json!({ "Secret": "sec" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "User": { "Id": "u2", "Name": "x" }, "AccessToken": "qc", "ServerId": "s"
            })))
            .mount(&server)
            .await;
        let mut c = Client::new(&server.uri(), info()).unwrap();
        let qc = c.quick_connect_initiate().await.unwrap();
        assert_eq!(qc.code, "123456");
        c.authenticate_quick_connect(&qc.secret).await.unwrap();
        assert_eq!(c.token(), Some("qc"));
    }

    #[tokio::test]
    async fn bad_password_is_distinct_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let mut c = Client::new(&server.uri(), info()).unwrap();
        assert!(matches!(c.authenticate("x", "y").await, Err(Error::BadCredentials)));
    }
}
