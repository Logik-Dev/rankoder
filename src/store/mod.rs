use std::collections::HashMap;

use sqlx::PgPool;

use crate::{
    models::drafts::{EpisodeDraft, MovieDraft, SeriesDraft},
    models::series::SeriesId,
    store::error::StoreError,
};

pub mod episode;
pub mod error;
pub mod media_file;
pub mod movie;
pub mod series;

pub struct MediaStore {
    pool: PgPool,
}

impl MediaStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert_movies_batched(
        &self,
        drafts: &[MovieDraft],
        batch_size: usize,
    ) -> Result<(), StoreError> {
        for chunk in drafts.chunks(batch_size) {
            let mut tx = self.pool.begin().await?;
            for draft in chunk {
                let movie_id = movie::find_or_create_movie(&mut tx, draft).await?;
                media_file::upsert_movie_file(&mut tx, movie_id, draft).await?;
            }
            tx.commit().await?;
        }
        Ok(())
    }

    pub async fn insert_series_batched(
        &self,
        drafts: &[SeriesDraft],
        batch_size: usize,
    ) -> Result<HashMap<String, SeriesId>, StoreError> {
        let mut map = HashMap::new();
        for chunk in drafts.chunks(batch_size) {
            let mut tx = self.pool.begin().await?;
            for draft in chunk {
                let jellyfin_id = draft.jellyfin_id.clone();
                let series_id = series::find_or_create_series(&mut tx, draft).await?;
                map.insert(jellyfin_id, series_id);
            }
            tx.commit().await?;
        }
        Ok(map)
    }

    pub async fn insert_episodes_batched(
        &self,
        drafts: &[EpisodeDraft],
        batch_size: usize,
    ) -> Result<(), StoreError> {
        for chunk in drafts.chunks(batch_size) {
            let mut tx = self.pool.begin().await?;
            for draft in chunk {
                let episode_id = episode::find_or_create_episode(&mut tx, draft).await?;
                media_file::upsert_episode_file(&mut tx, episode_id, draft).await?;
            }
            tx.commit().await?;
        }
        Ok(())
    }
}
