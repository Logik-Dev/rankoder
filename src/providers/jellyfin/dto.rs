use std::collections::HashMap;

use serde::Deserialize;

#[derive(Deserialize)]
pub(crate) struct JellyfinResponse {
    #[serde(rename = "Items")]
    pub items: Vec<JellyfinItem>,
    #[serde(rename = "TotalRecordCount")]
    pub total: u32,
}

#[derive(Deserialize, Debug)]
pub(crate) struct JellyfinItem {
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
