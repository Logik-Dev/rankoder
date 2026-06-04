use uuid::Uuid;

use crate::models::{
    common::{AbsoluteFilePath, Rating, TmdbId},
    provider_ids::ProviderIds,
};

#[derive(Debug, Clone)]
pub struct SeriesId(Uuid);

impl SeriesId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

#[derive(Debug)]
pub struct Series {
    pub id: SeriesId,
    pub title: String,
    pub tmdb_id: Option<TmdbId>,
    pub rating: Option<Rating>,
    pub path: Option<AbsoluteFilePath>,
    pub provider_ids: ProviderIds,
}
