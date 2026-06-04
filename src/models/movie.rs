use uuid::Uuid;

use crate::{
    impl_entity_id,
    models::common::{Rating, TmdbId},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, sqlx::Type)]
#[sqlx(transparent)]
pub struct MovieId(pub Uuid);

impl_entity_id!(MovieId);

#[derive(Debug)]
pub struct Movie {
    pub id: MovieId,
    pub title: String,
    pub tmdb_id: Option<TmdbId>,
    pub rating: Option<Rating>,
}
