use std::{collections::HashMap, fmt::Debug};

use crate::{
    models::{
        common::{AbsoluteFilePath, Rating, TmdbId},
        series::{Series, SeriesId},
    },
    providers::{SeriesProvider, error::ProviderError},
};
use async_trait::async_trait;
use axum::http::{HeaderMap, HeaderValue};
use futures::stream::{self, StreamExt, TryStreamExt};
use reqwest::{Client, Url};
use serde::Deserialize;
use tracing::{instrument, warn};

pub struct JellyfinProvider {
    http: Client,
    base_url: Url,
}

impl JellyfinProvider {
    #[instrument(skip(api_key))]
    pub fn new(url: impl AsRef<str> + Debug, api_key: &str) -> Result<Self, ProviderError> {
        let base_url = Url::parse(url.as_ref()).map_err(|_| ProviderError::InvalidUrl)?;

        let mut headers = HeaderMap::new();
        headers.insert("X-Emby-Token", HeaderValue::from_str(api_key)?);

        let http = Client::builder().default_headers(headers).build()?;

        Ok(Self { http, base_url })
    }

    async fn list_items(
        &self,
        start_index: i32,
        limit: i32,
        item_types: &str,
        fields: &str,
    ) -> Result<JellyfinResponse, ProviderError> {
        let url = self
            .base_url
            .join("Items")
            .map_err(|_| ProviderError::InvalidUrl)?;

        let resp = self
            .http
            .get(url)
            .query(&[
                ("IncludeItemTypes", item_types),
                ("Recursive", "true"),
                ("Fields", fields),
                ("StartIndex", &start_index.to_string()),
                ("Limit", &limit.to_string()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<JellyfinResponse>()
            .await?;

        Ok(resp)
    }

    async fn list_all_items_parallel(
        &self,
        item_types: &str,
        fields: &str,
    ) -> Result<Vec<JellyfinItem>, ProviderError> {
        const PAGE_SIZE: i32 = 200;
        const MAX_CONCURRENT: usize = 8;

        let first_page = self.list_items(0, PAGE_SIZE, item_types, fields).await?;
        let total = first_page.total as i32;
        let mut all_items = first_page.items;

        if total <= PAGE_SIZE {
            return Ok(all_items);
        }

        let remaining_starts = (PAGE_SIZE..total).step_by(PAGE_SIZE as usize);

        let remaining_pages: Vec<JellyfinResponse> = stream::iter(remaining_starts)
            .map(|start| self.list_items(start, PAGE_SIZE, item_types, fields))
            .buffer_unordered(MAX_CONCURRENT)
            .try_collect()
            .await?;

        for page in remaining_pages {
            all_items.extend(page.items);
        }

        Ok(all_items)
    }
}

#[async_trait]
impl SeriesProvider for JellyfinProvider {
    #[instrument(skip(self), err)]
    async fn list_series(&self) -> Result<Vec<Series>, ProviderError> {
        let items = self
            .list_all_items_parallel("Series", "ProviderIds,Path,CommunityRating")
            .await?;
        Ok(items.into_iter().map(Into::into).collect())
    }
}

#[derive(Deserialize)]
pub(super) struct JellyfinResponse {
    #[serde(rename = "Items")]
    pub items: Vec<JellyfinItem>,
    #[serde(rename = "TotalRecordCount")]
    pub total: u32,
}

#[derive(Deserialize, Debug)]
pub(super) struct JellyfinItem {
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Path")]
    pub path: Option<String>,
    #[serde(rename = "CommunityRating")]
    pub community_rating: Option<f32>,
    #[serde(rename = "ProviderIds")]
    pub provider_ids: HashMap<String, String>,
}

impl From<JellyfinItem> for Series {
    fn from(item: JellyfinItem) -> Self {
        let rating = item.community_rating.and_then(|r| {
            Rating::new(r)
                .inspect_err(|error| warn!(%error, "invalid rating value"))
                .ok()
        });
        let tmdb_id = item.provider_ids.get("Tmdb").and_then(|s| {
            s.parse::<TmdbId>()
                .inspect_err(|error| warn!(%error, "failed to parse imdb_id"))
                .ok()
        });
        let path = item.path.and_then(|p| {
            AbsoluteFilePath::new(&p)
                .inspect_err(|error| warn!(%error, "invalid path"))
                .ok()
        });

        Self {
            id: SeriesId::new(),
            jellyfin_id: item.id,
            title: item.name,
            rating,
            tmdb_id,
            path,
        }
    }
}
