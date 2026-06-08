use std::collections::HashMap;

use sqlx::PgPool;
use tracing::instrument;

use crate::{
    models::{
        drafts::{EpisodeDraft, MovieDraft, SeriesDraft},
        event::MediaEvent,
        media_file::{MediaFile, MediaFileId},
        series::SeriesId,
        video::VideoProperties,
        workflow::WorkflowStateTag,
    },
    store::{dto::MediaFileRow, error::StoreError},
};

pub mod dto;
pub mod episode;
pub mod error;
pub mod mapping;
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

    #[instrument(skip(self, drafts))]
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

    #[instrument(skip(self, drafts))]
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

    #[instrument(skip(self, drafts))]
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

    pub async fn find_media_file_by_id(
        &self,
        media_file_id: &MediaFileId,
    ) -> Result<MediaFile, StoreError> {
        let row = sqlx::query_as!(
            MediaFileRow,
            r#"
                SELECT id, episode_id, movie_id, file_path, size_bytes, video_codec, height, 
                width, bitrate_kbps, framerate, workflow_state as "workflow_state: WorkflowStateTag" 
                FROM media_files
                WHERE id = $1
            "#,
            media_file_id.as_uuid(),
        )
        .fetch_one(&self.pool)
        .await?;

        row.try_into()
    }

    pub async fn insert_probe_data(
        &self,
        media_file_id: &MediaFileId,
        video_properties: &VideoProperties,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let _ = sqlx::query!(
            r#"
                UPDATE media_files SET size_bytes = $1, video_codec = $2, height = $3, width = $4, bitrate_kbps = $5, framerate = $6, workflow_state = $7
                WHERE id = $8
            "#,
            video_properties.size_bytes.as_u64() as i64,
            video_properties.video_codec.as_ref(),
            video_properties.resolution.height() as i32,
            video_properties.resolution.width() as i32,
            video_properties.bitrate.as_ref().map(|b| b.as_bps() as i32),
            video_properties.framerate.as_ref().map(|f| f.to_string()),
            WorkflowStateTag::Probed as WorkflowStateTag,
            media_file_id.as_uuid()
        )
            .execute(&mut *tx)
            .await?;
        let event = MediaEvent::Probed;

        sqlx::query!(
            r#"
              INSERT INTO events(media_file_id, event) VALUES($1, $2)
            "#,
            media_file_id.as_uuid(),
            serde_json::to_value(event)?
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(())
    }
}
