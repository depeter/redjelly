use reqwest::{Method, RequestBuilder, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::json;
use url::Url;

use crate::models::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid Jellyseerr URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    /// No session, or it expired: sign in again.
    #[error("not signed in to Jellyseerr")]
    Unauthorized,
    #[error("wrong username or password")]
    BadCredentials,
    #[error("Jellyseerr returned {status} for {path}{}", message.as_deref().map(|m| format!(": {m}")).unwrap_or_default())]
    Status { status: StatusCode, path: String, message: Option<String> },
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum Sort {
    #[default]
    Popularity,
    Rating,
    Newest,
}

/// Server-side discover filters (TMDb `discover` via Jellyseerr).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiscoverQuery {
    pub genres: Vec<u32>,
    /// Every genre must match, rather than any of them.
    pub all_genres: bool,
    pub sort: Sort,
    /// `YYYY-MM-DD`, inclusive.
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    /// Minutes; movies only.
    pub runtime_min: Option<u32>,
    pub runtime_max: Option<u32>,
    pub vote_average_min: Option<f32>,
    pub vote_count_min: Option<u32>,
    /// TMDb watch region (`BE`) and provider ids; both or neither.
    pub watch_region: Option<String>,
    pub watch_providers: Vec<u32>,
}

impl DiscoverQuery {
    pub fn params(&self, kind: MediaKind, page: u32) -> Vec<(&'static str, String)> {
        let mut q = vec![("page", page.to_string())];
        let join = |ids: &[u32], sep: &str| ids.iter().map(u32::to_string).collect::<Vec<_>>().join(sep);
        if !self.genres.is_empty() {
            q.push(("genre", join(&self.genres, if self.all_genres { "," } else { "|" })));
        }
        q.push((
            "sortBy",
            match (self.sort, kind) {
                (Sort::Popularity, _) => "popularity.desc",
                (Sort::Rating, _) => "vote_average.desc",
                (Sort::Newest, MediaKind::Movie) => "primary_release_date.desc",
                (Sort::Newest, MediaKind::Tv) => "first_air_date.desc",
            }
            .into(),
        ));
        let (gte, lte) = match kind {
            MediaKind::Movie => ("primaryReleaseDateGte", "primaryReleaseDateLte"),
            MediaKind::Tv => ("firstAirDateGte", "firstAirDateLte"),
        };
        if let Some(d) = &self.date_from {
            q.push((gte, d.clone()));
        }
        if let Some(d) = &self.date_to {
            q.push((lte, d.clone()));
        }
        if kind == MediaKind::Movie {
            if let Some(m) = self.runtime_min {
                q.push(("withRuntimeGte", m.to_string()));
            }
            if let Some(m) = self.runtime_max {
                q.push(("withRuntimeLte", m.to_string()));
            }
        }
        if let Some(v) = self.vote_average_min {
            q.push(("voteAverageGte", v.to_string()));
        }
        if let Some(v) = self.vote_count_min {
            q.push(("voteCountGte", v.to_string()));
        }
        if let (Some(region), false) = (&self.watch_region, self.watch_providers.is_empty()) {
            q.push(("watchRegion", region.clone()));
            q.push(("watchProviders", join(&self.watch_providers, "|")));
        }
        q
    }
}

/// Cheap to clone; clones share the connection pool but not the session.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    /// `connect.sid=…` from the login response.
    cookie: Option<String>,
}

/// The `name=value` part of a `Set-Cookie` header.
fn session_cookie(set_cookie: &str) -> Option<String> {
    let pair = set_cookie.split(';').next()?.trim();
    pair.starts_with("connect.sid=").then(|| pair.to_owned())
}

impl Client {
    pub fn new(url: &str) -> Result<Self, Error> {
        let mut base = Url::parse(url.trim())?;
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        Ok(Self { http: reqwest::Client::new(), base: base.join("api/v1/")?, cookie: None })
    }

    pub fn has_session(&self) -> bool {
        self.cookie.is_some()
    }

    fn build(&self, method: Method, path: &str) -> Result<RequestBuilder, Error> {
        let mut rb = self.http.request(method, self.base.join(path.trim_start_matches('/'))?);
        if let Some(c) = &self.cookie {
            rb = rb.header("Cookie", c);
        }
        Ok(rb)
    }

    async fn send(rb: RequestBuilder, path: &str) -> Result<reqwest::Response, Error> {
        let resp = rb.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(Error::Unauthorized);
        }
        let message = resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_owned));
        Err(Error::Status { status, path: path.into(), message })
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T, Error> {
        let rb = self.build(Method::GET, path)?.query(query);
        Ok(Self::send(rb, path).await?.json().await?)
    }

    /// Signs in with Jellyfin credentials; Jellyseerr checks them against
    /// its Jellyfin server and starts a cookie session.
    pub async fn login_jellyfin(&mut self, username: &str, password: &str) -> Result<User, Error> {
        let path = "auth/jellyfin";
        let rb = self.build(Method::POST, path)?.json(&json!({ "username": username, "password": password }));
        let resp = match Self::send(rb, path).await {
            Err(Error::Unauthorized) => return Err(Error::BadCredentials),
            r => r?,
        };
        self.cookie = resp
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(session_cookie);
        if self.cookie.is_none() {
            tracing::warn!("Jellyseerr login returned no session cookie");
        }
        Ok(resp.json().await?)
    }

    pub async fn me(&self) -> Result<User, Error> {
        self.get("auth/me", &[]).await
    }

    pub async fn discover(&self, kind: MediaKind, query: &DiscoverQuery, page: u32) -> Result<Page, Error> {
        let path = match kind {
            MediaKind::Movie => "discover/movies",
            MediaKind::Tv => "discover/tv",
        };
        self.get(path, &query.params(kind, page)).await
    }

    pub async fn genres(&self, kind: MediaKind) -> Result<Vec<Genre>, Error> {
        self.get(&format!("genres/{}", kind.as_str()), &[]).await
    }

    pub async fn watch_providers(&self, kind: MediaKind, region: &str) -> Result<Vec<WatchProvider>, Error> {
        let path = match kind {
            MediaKind::Movie => "watchproviders/movies",
            MediaKind::Tv => "watchproviders/tv",
        };
        self.get(path, &[("watchRegion", region.to_owned())]).await
    }

    pub async fn details(&self, kind: MediaKind, id: u32) -> Result<Details, Error> {
        self.get(&format!("{}/{id}", kind.as_str()), &[]).await
    }

    /// IMDb + RT for movies, RT only for shows. Unknown titles give empty
    /// ratings rather than an error.
    pub async fn ratings(&self, kind: MediaKind, id: u32) -> Result<Ratings, Error> {
        let r = match kind {
            MediaKind::Movie => self.get::<CombinedRatings>(&format!("movie/{id}/ratingscombined"), &[]).await,
            MediaKind::Tv => self
                .get::<RtRating>(&format!("tv/{id}/ratings"), &[])
                .await
                .map(|rt| CombinedRatings { rt: Some(rt), imdb: None }),
        };
        match r {
            Ok(r) => Ok(r.into_ratings()),
            Err(Error::Status { status, .. }) if status == StatusCode::NOT_FOUND => Ok(Ratings::default()),
            Err(e) => Err(e),
        }
    }

    /// Requests a title; `seasons` is required for shows.
    pub async fn request(&self, kind: MediaKind, id: u32, seasons: &[u32]) -> Result<(), Error> {
        let mut body = json!({ "mediaType": kind.as_str(), "mediaId": id });
        if kind == MediaKind::Tv {
            body["seasons"] = json!(seasons);
        }
        let path = "request";
        Self::send(self.build(Method::POST, path)?.json(&body), path).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn query_params() {
        let q = DiscoverQuery {
            genres: vec![878, 53],
            date_from: Some("1990-01-01".into()),
            runtime_max: Some(100),
            vote_count_min: Some(500),
            watch_region: Some("BE".into()),
            watch_providers: vec![8, 337],
            sort: Sort::Newest,
            ..Default::default()
        };
        let movie = q.params(MediaKind::Movie, 2);
        assert!(movie.contains(&("genre", "878|53".into())));
        assert!(movie.contains(&("sortBy", "primary_release_date.desc".into())));
        assert!(movie.contains(&("primaryReleaseDateGte", "1990-01-01".into())));
        assert!(movie.contains(&("withRuntimeLte", "100".into())));
        assert!(movie.contains(&("watchProviders", "8|337".into())));
        let tv = q.params(MediaKind::Tv, 1);
        assert!(tv.contains(&("firstAirDateGte", "1990-01-01".into())));
        assert!(tv.contains(&("sortBy", "first_air_date.desc".into())));
        assert!(!tv.iter().any(|(k, _)| k.starts_with("withRuntime")));

        let all = DiscoverQuery { genres: vec![1, 2], all_genres: true, ..Default::default() };
        assert!(all.params(MediaKind::Movie, 1).contains(&("genre", "1,2".into())));
        // Providers without a region are ignored.
        let no_region = DiscoverQuery { watch_providers: vec![8], ..Default::default() };
        assert!(!no_region.params(MediaKind::Movie, 1).iter().any(|(k, _)| *k == "watchProviders"));
    }

    #[test]
    fn cookie_from_set_cookie() {
        assert_eq!(
            session_cookie("connect.sid=s%3Aabc.def; Path=/; Expires=Thu, 01 Jan 2027 00:00:00 GMT; HttpOnly").as_deref(),
            Some("connect.sid=s%3Aabc.def")
        );
        assert_eq!(session_cookie("other=1; Path=/"), None);
    }

    #[tokio::test]
    async fn login_then_discover_with_cookie() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/jellyfin"))
            .and(body_json(json!({ "username": "peter", "password": "pw" })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("set-cookie", "connect.sid=s%3Axyz; Path=/; HttpOnly")
                    .set_body_json(json!({ "id": 3, "displayName": "Peter", "jellyfinUsername": "peter" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/discover/tv"))
            .and(header("cookie", "connect.sid=s%3Axyz"))
            .and(query_param("page", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "page": 1, "totalPages": 7, "totalResults": 140,
                "results": [{
                    "id": 1399, "mediaType": "tv", "name": "Game of Thrones", "firstAirDate": "2011-04-17",
                    "posterPath": "/a.jpg", "voteAverage": 8.4, "voteCount": 20000, "genreIds": [18, 10765],
                    "originalLanguage": "en",
                    "mediaInfo": { "status": 5, "jellyfinMediaId": "abc123", "requests": [] }
                }]
            })))
            .mount(&server)
            .await;

        let mut c = Client::new(&server.uri()).unwrap();
        let user = c.login_jellyfin("peter", "pw").await.unwrap();
        assert_eq!(user.name(), Some("Peter"));
        assert!(c.has_session());
        let page = c.discover(MediaKind::Tv, &DiscoverQuery::default(), 1).await.unwrap();
        assert_eq!(page.total_pages, 7);
        let t = &page.results[0];
        assert_eq!(t.title, "Game of Thrones");
        assert_eq!(t.year(), Some(2011));
        let info = t.media_info.as_ref().unwrap();
        assert!(info.in_library() && !info.requested());
        assert_eq!(info.jellyfin_media_id.as_deref(), Some("abc123"));
    }

    #[tokio::test]
    async fn bad_login_and_expired_session() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/auth/jellyfin"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({ "message": "Unauthorized" })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/genres/movie"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let mut c = Client::new(&server.uri()).unwrap();
        assert!(matches!(c.login_jellyfin("x", "y").await, Err(Error::BadCredentials)));
        assert!(matches!(c.genres(MediaKind::Movie).await, Err(Error::Unauthorized)));
    }

    #[tokio::test]
    async fn ratings_movie_tv_and_missing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/movie/550/ratingscombined"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "rt": { "criticsScore": 79, "criticsRating": "Certified Fresh", "audienceScore": 96 },
                "imdb": { "criticsScore": 8.8, "url": "https://www.imdb.com/title/tt0137523" }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/tv/1399/ratings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "criticsScore": 89, "audienceScore": 0 })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v1/movie/1/ratingscombined"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({ "message": "Rotten Tomatoes ratings not found." })))
            .mount(&server)
            .await;
        let c = Client::new(&server.uri()).unwrap();
        let r = c.ratings(MediaKind::Movie, 550).await.unwrap();
        assert_eq!(r, Ratings { imdb: Some(8.8), rt_critics: Some(79), rt_audience: Some(96) });
        let r = c.ratings(MediaKind::Tv, 1399).await.unwrap();
        assert_eq!(r, Ratings { imdb: None, rt_critics: Some(89), rt_audience: None });
        assert!(c.ratings(MediaKind::Movie, 1).await.unwrap().is_empty());
    }

    #[test]
    fn movie_and_tv_details_parse() {
        let movie: Details = serde_json::from_value(json!({
            "id": 27205, "title": "Inception", "originalTitle": "Inception", "releaseDate": "2010-07-15",
            "tagline": "Your mind is the scene of the crime.", "runtime": 148, "budget": 160000000,
            "revenue": 825532764u64, "status": "Released", "voteCount": 35000,
            "productionCompanies": [{ "id": 923, "name": "Legendary Pictures", "logoPath": null }],
            "productionCountries": [{ "iso_3166_1": "US", "name": "United States of America" }],
            "relatedVideos": [
                { "url": "https://www.youtube.com/watch?v=clip", "type": "Clip", "site": "YouTube" },
                { "url": "https://www.youtube.com/watch?v=YoHD9XEInc0", "type": "Trailer", "site": "YouTube" }
            ],
            "credits": {
                "cast": [{ "id": 6193, "name": "Leonardo DiCaprio", "character": "Cobb", "profilePath": "/a.jpg" }],
                "crew": [{ "id": 525, "name": "Christopher Nolan", "job": "Director" }]
            },
            "watchProviders": [{ "iso_3166_1": "BE", "flatrate": [{ "id": 8, "name": "Netflix", "displayPriority": 1 }] }]
        }))
        .unwrap();
        assert_eq!(movie.trailer_url(), Some("https://www.youtube.com/watch?v=YoHD9XEInc0"));
        assert_eq!(movie.credits.cast[0].character.as_deref(), Some("Cobb"));
        assert_eq!(movie.watch_providers[0].flatrate[0].name, "Netflix");

        let tv: Details = serde_json::from_value(json!({
            "id": 1399, "name": "Game of Thrones", "originalName": "Game of Thrones",
            "firstAirDate": "2011-04-17", "lastAirDate": "2019-05-19", "numberOfEpisodes": 73,
            "createdBy": [{ "id": 1, "name": "David Benioff" }], "networks": [{ "id": 49, "name": "HBO" }],
            "episodeRunTime": [60], "relatedVideos": [{ "url": "https://www.youtube.com/watch?v=t", "type": "Teaser", "site": "YouTube" }]
        }))
        .unwrap();
        assert_eq!(tv.title, "Game of Thrones");
        assert_eq!(tv.year(), Some(2011));
        assert_eq!(tv.networks[0].name, "HBO");
        assert_eq!(tv.trailer_url(), Some("https://www.youtube.com/watch?v=t"));
    }

    #[tokio::test]
    async fn request_bodies() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/request"))
            .and(body_json(json!({ "mediaType": "tv", "mediaId": 1399, "seasons": [1, 2] })))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/request"))
            .and(body_json(json!({ "mediaType": "movie", "mediaId": 550 })))
            .respond_with(ResponseTemplate::new(409).set_body_json(json!({ "message": "Request for this media already exists." })))
            .mount(&server)
            .await;
        let c = Client::new(&server.uri()).unwrap();
        c.request(MediaKind::Tv, 1399, &[1, 2]).await.unwrap();
        let err = c.request(MediaKind::Movie, 550, &[]).await.unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }
}
