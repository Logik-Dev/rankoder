use uuid::Uuid;

use crate::models::{
    common::{AbsoluteFilePath, Rating, TmdbId},
    provider_ids::ProviderIds,
};

#[derive(Debug, Clone)]
pub struct MovieId(Uuid);

impl MovieId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

#[derive(Debug)]
pub struct Movie {
    pub id: MovieId,
    pub title: String,
    pub path: Option<AbsoluteFilePath>,
    pub tmdb_id: Option<TmdbId>,
    pub rating: Option<Rating>,
    pub provider_ids: ProviderIds,
}
