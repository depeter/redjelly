//! Stream URLs for the unencrypted live channels (VRT NWS, Ketnet Junior).
//!
//! VRT's media API wants a `vrtPlayerToken`, which it hands out for a
//! `playerInfo` JWT signed with a key from its web player's JavaScript. The
//! key is scraped the way the Kodi add-on does it and cached for a day.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;

use crate::channels::Channel;
use crate::client::{Client, Error};

const PLAYER: &str = "https://player.vrt.be/vrtmax/js/player-lib.js";
const TOKENS: &str = "https://media-services-public.vrt.be/vualto-video-aggregator-web/rest/external/v2/tokens";
const MEDIA: &str = "https://media-services-public.vrt.be/media-aggregator/v2/media-items/";
const KEY_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerKey {
    pub kid: String,
    pub secret: String,
    pub version: String,
}

fn b64url(raw: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw)
}

/// HS256 JWT describing a desktop Firefox with the web player.
pub fn player_info(key: &PlayerKey, now: f64) -> String {
    let header = b64url(json!({ "alg": "HS256", "kid": key.kid }).to_string().as_bytes());
    let payload = b64url(
        json!({
            "drm": { "widevine": "L3" },
            "exp": ((now + 3600.0) * 1000.0).round() / 1000.0,
            "platform": "desktop",
            "app": { "type": "browser", "name": "Firefox", "version": "137.0" },
            "device": "undefined (undefined)",
            "os": { "name": "Linux", "version": "x86_64" },
            "player": { "name": "VRT web player", "version": key.version },
        })
        .to_string()
        .as_bytes(),
    );
    let signing = format!("{header}.{payload}");
    let mut mac = Hmac::<Sha256>::new_from_slice(key.secret.as_bytes()).expect("any key length");
    mac.update(signing.as_bytes());
    format!("{signing}.{}", b64url(&mac.finalize().into_bytes()))
}

/// Chunk paths `player-lib.js` imports.
fn chunks(js: &str) -> Vec<String> {
    let re = regex::Regex::new(r#""\.(/[a-z0-9-/]+[a-z0-9]{8}\.js)";"#).unwrap();
    re.captures_iter(js).map(|c| c[1].to_owned()).collect()
}

fn sub_chunks(js: &str) -> Vec<String> {
    let re = regex::Regex::new(r#"import\("\.(/[a-z0-9-]+\.js)"\)"#).unwrap();
    re.captures_iter(js).map(|c| c[1].to_owned()).collect()
}

/// The key from the DRM / bootstrapper chunks: reversed base64 in `atob()`
/// calls, the first being the key id and the last the secret.
pub fn key_from_chunk(js: &str) -> Option<PlayerKey> {
    let atob = regex::Regex::new(r#"atob\("(==[A-Za-z0-9+/]*)""#).unwrap();
    let found: Vec<_> = atob.captures_iter(js).map(|c| c[1].to_owned()).collect();
    let decode = |s: &str| {
        let rev: String = s.chars().rev().collect();
        String::from_utf8(base64::engine::general_purpose::STANDARD.decode(rev).ok()?).ok()
    };
    let version = regex::Regex::new(r#"\s"(\d\.\d\.\d-[a-zA-Z0-9\-:]*)""#)
        .unwrap()
        .captures(js)
        .map_or_else(|| "5.2.2".to_owned(), |c| c[1].to_owned());
    Some(PlayerKey { kid: decode(found.first()?)?, secret: decode(found.last()?)?, version })
}

async fn text(c: &Client, url: &str) -> Result<String, Error> {
    let resp = c.http.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(Error::Status(resp.status()));
    }
    Ok(resp.text().await?)
}

async fn scrape_key(c: &Client) -> Result<PlayerKey, Error> {
    let folder = PLAYER.rsplit_once('/').map_or(PLAYER, |(f, _)| f);
    let main = text(c, PLAYER).await?;
    let wanted = |p: &str| p.contains("drm") || p.contains("bootstrapper");
    let mut candidates = Vec::new();
    for p in chunks(&main) {
        if wanted(&p) {
            candidates.push(p.clone());
        }
        // The DRM code may sit one import deeper.
        if let Ok(body) = text(c, &format!("{folder}{p}")).await {
            candidates.extend(sub_chunks(&body).into_iter().filter(|s| wanted(s)));
        }
    }
    candidates.dedup();
    for p in candidates {
        if let Some(k) = key_from_chunk(&text(c, &format!("{folder}{p}")).await?) {
            return Ok(k);
        }
    }
    Err(Error::Api("player key not found in VRT's player script".into()))
}

/// The key, from `cache` when it's under a day old. A stale cache still
/// beats nothing when VRT's script changed shape.
pub async fn player_key(c: &Client, cache: &Path) -> Result<PlayerKey, Error> {
    let cached = std::fs::read_to_string(cache).ok().and_then(|s| serde_json::from_str::<PlayerKey>(&s).ok());
    let fresh = std::fs::metadata(cache)
        .and_then(|m| m.modified())
        .is_ok_and(|t| SystemTime::now().duration_since(t).unwrap_or_default() < KEY_MAX_AGE);
    if let (Some(k), true) = (&cached, fresh) {
        return Ok(k.clone());
    }
    match scrape_key(c).await {
        Ok(k) => {
            if let Some(dir) = cache.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(cache, serde_json::to_string(&k).unwrap_or_default());
            Ok(k)
        }
        Err(e) => cached.ok_or(e),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaItem {
    #[serde(default)]
    target_urls: Vec<TargetUrl>,
    drm: Option<String>,
    code: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TargetUrl {
    #[serde(rename = "type")]
    kind: String,
    url: String,
}

/// The playable URL in a media-items response: clear HLS only.
fn pick(v: Value) -> Result<String, Error> {
    let item: MediaItem = serde_json::from_value(v).map_err(|e| Error::Api(e.to_string()))?;
    if let Some(code) = item.code {
        return Err(match code.as_str() {
            "INVALID_LOCATION" => Error::GeoBlocked,
            "AUTHENTICATION_REQUIRED" => Error::LoginRequired,
            _ => Error::Api(item.message.unwrap_or(code)),
        });
    }
    if item.drm.is_some() {
        return Err(Error::Api("this stream is DRM protected".into()));
    }
    item.target_urls
        .into_iter()
        .find(|t| t.kind == "hls")
        .map(|t| t.url)
        .ok_or_else(|| Error::Api("no HLS stream".into()))
}

/// HLS URL for a clear live channel. `cache` holds the player key.
pub async fn live_url(c: &Client, channel: &Channel, cache: &Path) -> Result<String, Error> {
    let key = player_key(c, cache).await?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64();
    let resp: Value = c.http.post(TOKENS).json(&json!({ "playerInfo": player_info(&key, now) })).send().await?.json().await?;
    let Some(token) = resp["vrtPlayerToken"].as_str() else {
        // A wrong key: drop the cache so the next try scrapes again.
        let _ = std::fs::remove_file(cache);
        return Err(Error::Api(format!("no player token: {}", resp["message"].as_str().unwrap_or("?"))));
    };
    let url = format!("{MEDIA}{}", channel.stream_id);
    let item: Value = c
        .http
        .get(url)
        .query(&[("vrtPlayerToken", token), ("client", "vrtnu-web@PROD")])
        .send()
        .await?
        .json()
        .await?;
    pick(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn picks_clear_hls_only() {
        let url = pick(fixture("vualto_nieuws")).unwrap();
        assert!(url.starts_with("https://live.vrtcdn.be/") && url.contains(".m3u8"));
        assert!(matches!(pick(fixture("vualto_een_geo")), Err(Error::Api(m)) if m.contains("DRM")));
        let geo = json!({ "code": "INVALID_LOCATION", "message": "nope" });
        assert!(matches!(pick(geo), Err(Error::GeoBlocked)));
    }

    #[test]
    fn signs_player_info() {
        let key = PlayerKey { kid: "kid1".into(), secret: "s3cret".into(), version: "5.2.2".into() };
        let jwt = player_info(&key, 1_000_000.0);
        let parts: Vec<_> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let dec = |s: &str| -> Value {
            serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).unwrap()).unwrap()
        };
        assert_eq!(dec(parts[0]), json!({ "alg": "HS256", "kid": "kid1" }));
        assert_eq!(dec(parts[1])["exp"], json!(1_003_600.0));
        let mut mac = Hmac::<Sha256>::new_from_slice(b"s3cret").unwrap();
        mac.update(format!("{}.{}", parts[0], parts[1]).as_bytes());
        mac.verify_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
    }

    #[test]
    fn scrapes_key_and_chunks() {
        let rev = |s: &str| base64::engine::general_purpose::STANDARD.encode(s).chars().rev().collect::<String>();
        // Real keys pad to a leading "==" once reversed.
        let js = format!(r#"x=atob("{}");y=atob("{}"); v= "5.9.1-abc""#, rev("kid-123"), rev("secret-abcd12"));
        let k = key_from_chunk(&js).unwrap();
        assert_eq!((k.kid.as_str(), k.secret.as_str(), k.version.as_str()), ("kid-123", "secret-abcd12", "5.9.1-abc"));
        assert_eq!(key_from_chunk("nothing here"), None);
        let main = r#"a="./chunks/drm-abcd1234.js";b="./chunks/ui-1234abcd.js";"#;
        assert_eq!(chunks(main), vec!["/chunks/drm-abcd1234.js", "/chunks/ui-1234abcd.js"]);
        assert_eq!(sub_chunks(r#"import("./bootstrapper-x.js")"#), vec!["/bootstrapper-x.js"]);
    }

    #[tokio::test]
    #[ignore]
    async fn live_nws() {
        let dir = std::env::temp_dir().join(format!("rj-vrt-test-{}", std::process::id()));
        let url = live_url(&Client::new(), Channel::by_slug("vrt-nws").unwrap(), &dir.join("key.json")).await.unwrap();
        assert!(url.contains(".m3u8"));
    }
}
