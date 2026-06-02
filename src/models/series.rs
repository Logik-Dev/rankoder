use uuid::Uuid;

use crate::models::common::{AbsoluteFilePath, Rating, TmdbId};

#[derive(Debug)]
pub struct SeriesId(Uuid);

impl SeriesId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

#[derive(Debug)]
pub struct Series {
    pub id: SeriesId,
    pub jellyfin_id: String,
    pub title: String,
    pub tmdb_id: Option<TmdbId>,
    pub rating: Option<Rating>,
    pub path: Option<AbsoluteFilePath>,
}
