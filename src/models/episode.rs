use uuid::Uuid;

use crate::models::{
    common::{AbsoluteFilePath, EpisodeNumber, Rating, SeasonNumber, TmdbId},
    provider_ids::ProviderIds,
    series::SeriesId,
};

#[derive(Debug, Clone)]
pub struct EpisodeId(Uuid);

impl EpisodeId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

#[derive(Debug)]
pub struct Episode {
    pub id: EpisodeId,
    pub series_id: SeriesId,
    pub season_number: Option<SeasonNumber>,
    pub episode_number: Option<EpisodeNumber>,
    pub title: String,
    pub path: Option<AbsoluteFilePath>,
    pub tmdb_id: Option<TmdbId>,
    pub rating: Option<Rating>,
    pub provider_ids: ProviderIds,
}
