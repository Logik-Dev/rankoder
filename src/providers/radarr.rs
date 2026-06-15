use async_trait::async_trait;
use axum::http::{HeaderMap, HeaderValue};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use crate::providers::{MediaServerNotifier, error::ProviderError};

/// Talks to a Radarr instance over its v3 REST API to trigger a per-movie disk
/// rescan after a transcode. Auth uses the `X-Api-Key` header.
pub struct RadarrClient {
    http: Client,
    base_url: Url,
}

impl RadarrClient {
    pub fn new(url: &str, api_key: &str) -> Result<Self, ProviderError> {
        let base_url = Url::parse(url).map_err(|_| ProviderError::InvalidUrl)?;

        let mut headers = HeaderMap::new();
        headers.insert("X-Api-Key", HeaderValue::from_str(api_key)?);

        let http = Client::builder().default_headers(headers).build()?;

        Ok(Self { http, base_url })
    }
}

#[derive(Deserialize)]
struct RadarrMovie {
    id: i64,
}

#[derive(Serialize)]
struct RescanCommand {
    name: &'static str,
    #[serde(rename = "movieId")]
    movie_id: i64,
}

#[async_trait]
impl MediaServerNotifier for RadarrClient {
    #[instrument(skip(self), err)]
    async fn refresh_movie(&self, tmdb_id: i32) -> Result<(), ProviderError> {
        // Resolve our TMDB id to Radarr's internal movie id.
        let movie_url = self
            .base_url
            .join("api/v3/movie")
            .map_err(|_| ProviderError::InvalidUrl)?;

        let movies: Vec<RadarrMovie> = self
            .http
            .get(movie_url)
            .query(&[("tmdbId", tmdb_id.to_string())])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let Some(movie) = movies.into_iter().next() else {
            return Err(ProviderError::TmdbIdNotFound);
        };

        // RescanMovie re-reads the movie folder from disk (picking up the new
        // file and its media info) without refetching metadata from TMDB.
        let command_url = self
            .base_url
            .join("api/v3/command")
            .map_err(|_| ProviderError::InvalidUrl)?;

        self.http
            .post(command_url)
            .json(&RescanCommand {
                name: "RescanMovie",
                movie_id: movie.id,
            })
            .send()
            .await?
            .error_for_status()?;

        debug!(tmdb_id, radarr_movie_id = movie.id, "requested Radarr rescan");
        Ok(())
    }
}
