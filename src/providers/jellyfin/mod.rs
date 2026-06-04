mod dto;
mod mapping;
mod series;

use std::fmt::Debug;

use axum::http::{HeaderMap, HeaderValue};
use futures::stream::{self, StreamExt, TryStreamExt};
use reqwest::{Client, Url};

use crate::providers::error::ProviderError;

pub(crate) use dto::{JellyfinItem, JellyfinResponse};

pub struct JellyfinProvider {
    http: Client,
    base_url: Url,
}

impl JellyfinProvider {
    pub fn new(url: impl AsRef<str> + Debug, api_key: &str) -> Result<Self, ProviderError> {
        let base_url = Url::parse(url.as_ref()).map_err(|_| ProviderError::InvalidUrl)?;

        let mut headers = HeaderMap::new();
        headers.insert("X-Emby-Token", HeaderValue::from_str(api_key)?);

        let http = Client::builder().default_headers(headers).build()?;

        Ok(Self { http, base_url })
    }

    pub(crate) async fn list_all_items_parallel(
        &self,
        item_types: &str,
        fields: &str,
        parent_id: Option<&str>,
    ) -> Result<Vec<JellyfinItem>, ProviderError> {
        const PAGE_SIZE: i32 = 200;
        const MAX_CONCURRENT: usize = 8;

        let first_page = self
            .list_items(0, PAGE_SIZE, item_types, fields, parent_id)
            .await?;
        let total = first_page.total as i32;
        let mut all_items = first_page.items;

        if total <= PAGE_SIZE {
            return Ok(all_items);
        }

        let remaining_starts = (PAGE_SIZE..total).step_by(PAGE_SIZE as usize);

        let remaining_pages: Vec<JellyfinResponse> = stream::iter(remaining_starts)
            .map(|start| self.list_items(start, PAGE_SIZE, item_types, fields, parent_id))
            .buffer_unordered(MAX_CONCURRENT)
            .try_collect()
            .await?;

        for page in remaining_pages {
            all_items.extend(page.items);
        }

        Ok(all_items)
    }

    async fn list_items(
        &self,
        start_index: i32,
        limit: i32,
        item_types: &str,
        fields: &str,
        parent_id: Option<&str>,
    ) -> Result<JellyfinResponse, ProviderError> {
        let url = self
            .base_url
            .join("Items")
            .map_err(|_| ProviderError::InvalidUrl)?;

        let start_str = start_index.to_string();
        let limit_str = limit.to_string();

        let mut params = vec![
            ("IncludeItemTypes", item_types),
            ("Recursive", "true"),
            ("Fields", fields),
            ("StartIndex", &start_str),
            ("Limit", &limit_str),
        ];

        if let Some(pid) = parent_id {
            params.push(("ParentId", pid));
        }

        let resp = self
            .http
            .get(url)
            .query(&params)
            .send()
            .await?
            .error_for_status()?
            .json::<JellyfinResponse>()
            .await?;

        Ok(resp)
    }
}
