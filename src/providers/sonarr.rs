use async_trait::async_trait;
use axum::http::{HeaderMap, HeaderValue};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};

use crate::providers::{SeriesNotifier, error::ProviderError};

/// Talks to a Sonarr instance over its v3 REST API to trigger a per-series disk
/// rescan after a transcode. Auth uses the `X-Api-Key` header.
pub struct SonarrClient {
    http: Client,
    base_url: Url,
}

impl SonarrClient {
    pub fn new(url: &str, api_key: &str) -> Result<Self, ProviderError> {
        let base_url = Url::parse(url).map_err(|_| ProviderError::InvalidUrl)?;

        let mut headers = HeaderMap::new();
        headers.insert("X-Api-Key", HeaderValue::from_str(api_key)?);

        let http = Client::builder().default_headers(headers).build()?;

        Ok(Self { http, base_url })
    }
}

#[derive(Deserialize)]
struct SonarrSeries {
    id: i64,
}

#[derive(Serialize)]
struct RescanCommand {
    name: &'static str,
    #[serde(rename = "seriesId")]
    series_id: i64,
}

#[async_trait]
impl SeriesNotifier for SonarrClient {
    #[instrument(skip(self), err)]
    async fn refresh_series(&self, tvdb_id: i32) -> Result<(), ProviderError> {
        // Resolve our TVDB id to Sonarr's internal series id.
        let series_url = self
            .base_url
            .join("api/v3/series")
            .map_err(|_| ProviderError::InvalidUrl)?;

        let series: Vec<SonarrSeries> = self
            .http
            .get(series_url)
            .query(&[("tvdbId", tvdb_id.to_string())])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let Some(series) = series.into_iter().next() else {
            return Err(ProviderError::TvdbIdNotFound);
        };

        // RescanSeries re-reads the series folder from disk (picking up the new
        // file and its media info) without refetching metadata.
        let command_url = self
            .base_url
            .join("api/v3/command")
            .map_err(|_| ProviderError::InvalidUrl)?;

        self.http
            .post(command_url)
            .json(&RescanCommand {
                name: "RescanSeries",
                series_id: series.id,
            })
            .send()
            .await?
            .error_for_status()?;

        debug!(tvdb_id, sonarr_series_id = series.id, "requested Sonarr rescan");
        Ok(())
    }
}
