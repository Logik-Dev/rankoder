use uuid::Uuid;

use crate::{
    impl_entity_id,
    models::{
        common::{EpisodeNumber, Rating, SeasonNumber, TmdbId},
        series::SeriesId,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct EpisodeId(pub Uuid);

impl_entity_id!(EpisodeId);

#[derive(Debug)]
pub struct Episode {
    pub id: EpisodeId,
    pub series_id: SeriesId,
    pub season_number: Option<SeasonNumber>,
    pub episode_number: Option<EpisodeNumber>,
    pub title: String,
    pub tmdb_id: Option<TmdbId>,
    pub rating: Option<Rating>,
}
