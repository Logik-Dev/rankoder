use uuid::Uuid;

use crate::{
    impl_entity_id,
    models::common::{Rating, TmdbId},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct SeriesId(pub Uuid);

impl_entity_id!(SeriesId);

#[derive(Debug)]
pub struct Series {
    pub id: SeriesId,
    pub title: String,
    pub tmdb_id: Option<TmdbId>,
    pub rating: Option<Rating>,
}
