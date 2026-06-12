use std::collections::HashMap;

use sqlx::PgPool;
use tracing::instrument;

use crate::{
    models::{
        drafts::{EpisodeDraft, MovieDraft, SeriesDraft},
        event::MediaEvent,
        media_file::{MediaFile, MediaFileId},
        series::SeriesId,
        transcode::TranscodeDecision,
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
                width, bitrate_kbps, framerate, duration_seconds, workflow_state as "workflow_state: WorkflowStateTag"
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
        let result = sqlx::query!(
            r#"
                UPDATE media_files
                SET size_bytes = $1, video_codec = $2, height = $3, width = $4,
                    bitrate_kbps = $5, framerate = $6, duration_seconds = $7, workflow_state = $8
                WHERE id = $9 AND workflow_state = $10
            "#,
            video_properties.size_bytes.as_u64() as i64,
            video_properties.video_codec.as_ref(),
            video_properties.resolution.height() as i32,
            video_properties.resolution.width() as i32,
            video_properties.bitrate.as_ref().map(|b| b.as_bps() as i32),
            video_properties.framerate.as_ref().map(|f| f.to_string()),
            video_properties.duration.as_ref().map(|d| d.as_secs_f64()),
            WorkflowStateTag::Probed as WorkflowStateTag,
            media_file_id.as_uuid(),
            WorkflowStateTag::Discovered as WorkflowStateTag,
        )
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() == 0 {
            return Err(StoreError::StaleState {
                expected: WorkflowStateTag::Discovered,
            });
        }

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

    pub async fn fetch_tmdb_rating_for_file(
        &self,
        media_file_id: &MediaFileId,
    ) -> Result<Option<f32>, StoreError> {
        let row = sqlx::query!(
            r#"
                SELECT COALESCE(e.rating, m.rating) AS rating
                FROM media_files mf
                LEFT JOIN episodes e ON mf.episode_id = e.id
                LEFT JOIN movies   m ON mf.movie_id   = m.id
                WHERE mf.id = $1
            "#,
            media_file_id.as_uuid(),
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(row.rating)
    }

    pub async fn fetch_active_media_files(&self) -> Result<Vec<MediaFileId>, StoreError> {
        let rows = sqlx::query!(
            r#"SELECT id FROM media_files WHERE workflow_state NOT IN ('done', 'skipped', 'failed')"#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| MediaFileId::from(r.id)).collect())
    }

    pub async fn transition(
        &self,
        media_file_id: &MediaFileId,
        from: WorkflowStateTag,
        to: WorkflowStateTag,
        event: &MediaEvent,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;

        let result = sqlx::query!(
            r#"UPDATE media_files SET workflow_state = $1 WHERE id = $2 AND workflow_state = $3"#,
            to as WorkflowStateTag,
            media_file_id.as_uuid(),
            from as WorkflowStateTag,
        )
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() == 0 {
            return Err(StoreError::StaleState { expected: from });
        }

        sqlx::query!(
            r#"INSERT INTO events(media_file_id, event) VALUES($1, $2)"#,
            media_file_id.as_uuid(),
            serde_json::to_value(event)?,
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn save_analysis_result(
        &self,
        media_file_id: &MediaFileId,
        decision: &TranscodeDecision,
    ) -> Result<(), StoreError> {
        match decision {
            TranscodeDecision::Encode {
                bpp,
                compression_potential,
                crf,
            } => {
                let mut tx = self.pool.begin().await?;
                let spec = serde_json::json!({ "crf": crf });
                let result = sqlx::query!(
                    r#"
                        UPDATE media_files
                        SET workflow_state = $1, transcode_spec = $2
                        WHERE id = $3 AND workflow_state = $4
                    "#,
                    WorkflowStateTag::Analyzed as WorkflowStateTag,
                    spec,
                    media_file_id.as_uuid(),
                    WorkflowStateTag::Probed as WorkflowStateTag,
                )
                .execute(&mut *tx)
                .await?;

                if result.rows_affected() == 0 {
                    return Err(StoreError::StaleState {
                        expected: WorkflowStateTag::Probed,
                    });
                }

                sqlx::query!(
                    r#"INSERT INTO events(media_file_id, event) VALUES($1, $2)"#,
                    media_file_id.as_uuid(),
                    serde_json::to_value(MediaEvent::Analyzed {
                        bpp: *bpp,
                        compression_potential: *compression_potential,
                        crf: *crf,
                    })?
                )
                .execute(&mut *tx)
                .await?;

                tx.commit().await?;
                Ok(())
            }
            TranscodeDecision::Skip(reason) => {
                self.transition(
                    media_file_id,
                    WorkflowStateTag::Probed,
                    WorkflowStateTag::Skipped,
                    &MediaEvent::Skipped {
                        reason: reason.clone(),
                        bpp: None,
                        compression_potential: None,
                    },
                )
                .await
            }
            TranscodeDecision::SkipWithAnalysis {
                reason,
                bpp,
                compression_potential,
            } => {
                self.transition(
                    media_file_id,
                    WorkflowStateTag::Probed,
                    WorkflowStateTag::Skipped,
                    &MediaEvent::Skipped {
                        reason: reason.clone(),
                        bpp: Some(*bpp),
                        compression_potential: Some(*compression_potential),
                    },
                )
                .await
            }
        }
    }

    pub async fn fetch_approval_info(
        &self,
        media_file_id: &MediaFileId,
    ) -> Result<ApprovalInfo, StoreError> {
        let row = sqlx::query!(
            r#"
                SELECT
                    COALESCE(e.title, m.title) AS title,
                    COALESCE(e.rating, m.rating) AS tmdb_rating,
                    (mf.transcode_spec->>'crf')::integer AS crf,
                    (
                        SELECT (ev.event->>'compression_potential')::double precision
                        FROM events ev
                        WHERE ev.media_file_id = mf.id
                          AND ev.event->>'type' = 'analyzed'
                        ORDER BY ev.created_at DESC
                        LIMIT 1
                    ) AS compression_potential
                FROM media_files mf
                LEFT JOIN episodes e ON mf.episode_id = e.id
                LEFT JOIN movies   m ON mf.movie_id   = m.id
                WHERE mf.id = $1
            "#,
            media_file_id.as_uuid(),
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(ApprovalInfo {
            title: row.title,
            tmdb_rating: row.tmdb_rating,
            crf: row.crf,
            compression_potential: row.compression_potential,
        })
    }
}

pub struct ApprovalInfo {
    pub title: Option<String>,
    pub tmdb_rating: Option<f32>,
    pub crf: Option<i32>,
    pub compression_potential: Option<f64>,
}
