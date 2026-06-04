use std::collections::HashMap;

use serde::Deserialize;

use crate::providers::ParentId;

#[derive(Deserialize)]
pub struct JellyfinResponse {
    #[serde(rename = "Items")]
    pub items: Vec<JellyfinItem>,
    #[serde(rename = "TotalRecordCount")]
    pub total: u32,
}

#[derive(Deserialize, Debug)]
pub struct JellyfinItem {
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
    #[serde(rename = "IndexNumber")]
    pub index_number: Option<i32>,
    #[serde(rename = "ParentIndexNumber")]
    pub parent_index_number: Option<i32>,
    #[serde(rename = "SeriesId")]
    pub series_id: Option<String>,
}

impl ParentId for JellyfinItem {
    fn parent_id(&self) -> Option<&str> {
        self.series_id.as_deref()
    }
}
