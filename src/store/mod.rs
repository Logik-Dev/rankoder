use std::collections::HashMap;

use sqlx::PgPool;
use tracing::instrument;
use uuid::Uuid;

use crate::{
    models::{
        batch::BatchKey,
        drafts::{EpisodeDraft, MovieDraft, SeriesDraft},
        event::MediaEvent,
        media_file::{MediaFile, MediaFileId},
        movie::MovieId,
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
                let spec = serde_json::json!({
                    "crf": crf,
                    "bpp": bpp,
                    "compression_potential": compression_potential,
                });
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

    pub async fn count_pending_batches(&self) -> Result<i64, StoreError> {
        let row = sqlx::query!(
            r#"
            SELECT (
                (SELECT COUNT(DISTINCT (e.series_id, e.season_number))
                 FROM media_files mf JOIN episodes e ON mf.episode_id = e.id
                 WHERE mf.workflow_state = 'pending_approval')
              + (SELECT COUNT(*) FROM media_files
                 WHERE movie_id IS NOT NULL AND workflow_state = 'pending_approval')
            ) AS count
            "#
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(row.count.unwrap_or(0))
    }

    pub async fn fetch_ready_batch_keys(&self, limit: i64) -> Result<Vec<BatchKey>, StoreError> {
        let rows = sqlx::query!(
            r#"
            SELECT 'season' AS kind, e.series_id, e.season_number AS season, NULL::uuid AS movie_id,
                   MIN(mf.updated_at) AS oldest
            FROM media_files mf JOIN episodes e ON mf.episode_id = e.id
            GROUP BY e.series_id, e.season_number
            HAVING bool_or(mf.workflow_state = 'analyzed')
               AND NOT bool_or(mf.workflow_state IN ('discovered','probed','pending_approval'))
            UNION ALL
            SELECT 'movie', NULL::uuid, NULL::smallint, mf.movie_id, mf.updated_at
            FROM media_files mf
            WHERE mf.movie_id IS NOT NULL AND mf.workflow_state = 'analyzed'
            ORDER BY oldest ASC
            LIMIT $1
            "#,
            limit,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| {
                if r.kind.as_deref() == Some("season") {
                    BatchKey::Season {
                        series_id: SeriesId::from(r.series_id.unwrap()),
                        season: r.season.unwrap(),
                    }
                } else {
                    BatchKey::Movie {
                        movie_id: MovieId::from(r.movie_id.unwrap()),
                    }
                }
            })
            .collect())
    }

    pub async fn fetch_stale_pending_batches(
        &self,
        threshold_minutes: i32,
    ) -> Result<Vec<BatchKey>, StoreError> {
        let rows = sqlx::query!(
            r#"
            SELECT 'season' AS kind, e.series_id, e.season_number AS season, NULL::uuid AS movie_id,
                   MIN(mf.updated_at) AS oldest
            FROM media_files mf JOIN episodes e ON mf.episode_id = e.id
            WHERE mf.workflow_state = 'pending_approval'
            GROUP BY e.series_id, e.season_number
            HAVING MIN(mf.updated_at) < NOW() - make_interval(mins => $1)
            UNION ALL
            SELECT 'movie', NULL::uuid, NULL::smallint, mf.movie_id, mf.updated_at
            FROM media_files mf
            WHERE mf.movie_id IS NOT NULL
              AND mf.workflow_state = 'pending_approval'
              AND mf.updated_at < NOW() - make_interval(mins => $1)
            ORDER BY oldest ASC
            "#,
            threshold_minutes,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| {
                if r.kind.as_deref() == Some("season") {
                    BatchKey::Season {
                        series_id: SeriesId::from(r.series_id.unwrap()),
                        season: r.season.unwrap(),
                    }
                } else {
                    BatchKey::Movie {
                        movie_id: MovieId::from(r.movie_id.unwrap()),
                    }
                }
            })
            .collect())
    }

    pub async fn transition_batch(
        &self,
        key: &BatchKey,
        from: WorkflowStateTag,
        to: WorkflowStateTag,
        event: &MediaEvent,
    ) -> Result<Vec<MediaFileId>, StoreError> {
        let mut tx = self.pool.begin().await?;

        let ids: Vec<Uuid> = match key {
            BatchKey::Season { series_id, season } => sqlx::query!(
                r#"
                    UPDATE media_files mf SET workflow_state = $1
                    FROM episodes e
                    WHERE mf.episode_id = e.id
                      AND e.series_id = $2
                      AND e.season_number = $3
                      AND mf.workflow_state = $4
                    RETURNING mf.id
                    "#,
                to as WorkflowStateTag,
                series_id.as_uuid(),
                *season,
                from as WorkflowStateTag,
            )
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|r| r.id)
            .collect(),
            BatchKey::Movie { movie_id } => sqlx::query!(
                r#"
                    UPDATE media_files SET workflow_state = $1
                    WHERE movie_id = $2
                      AND workflow_state = $3
                    RETURNING id
                    "#,
                to as WorkflowStateTag,
                movie_id.as_uuid(),
                from as WorkflowStateTag,
            )
            .fetch_all(&mut *tx)
            .await?
            .into_iter()
            .map(|r| r.id)
            .collect(),
        };

        if ids.is_empty() {
            tx.rollback().await?;
            return Ok(Vec::new());
        }

        sqlx::query!(
            r#"
            INSERT INTO events(media_file_id, event)
            SELECT unnest($1::uuid[]), $2
            "#,
            &ids[..],
            serde_json::to_value(event)?,
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(ids.into_iter().map(MediaFileId::from).collect())
    }

    pub async fn fetch_batch_request_info(
        &self,
        key: &BatchKey,
    ) -> Result<BatchApprovalInfo, StoreError> {
        match key {
            BatchKey::Season { series_id, season } => {
                let row = sqlx::query!(
                    r#"
                    SELECT s.title, s.rating,
                           COUNT(*)::bigint AS file_count,
                           SUM(mf.size_bytes)::bigint AS total_size_bytes,
                           SUM(mf.size_bytes * LEAST(GREATEST(COALESCE((mf.transcode_spec->>'compression_potential')::float8, 0), 0), 1))::bigint AS saved_bytes
                    FROM media_files mf
                    JOIN episodes e ON mf.episode_id = e.id
                    JOIN series s ON e.series_id = s.id
                    WHERE e.series_id = $1
                      AND e.season_number = $2
                      AND mf.workflow_state = 'pending_approval'
                    GROUP BY s.title, s.rating
                    "#,
                    series_id.as_uuid(),
                    *season,
                )
                .fetch_one(&self.pool)
                .await?;

                let total_size_gb = bytes_to_gb(row.total_size_bytes.unwrap_or(0));
                let saved_gb = bytes_to_gb(row.saved_bytes.unwrap_or(0));

                Ok(BatchApprovalInfo {
                    title: format!("{} — Saison {}", row.title, season),
                    tmdb_rating: row.rating,
                    file_count: row.file_count.unwrap_or(0),
                    total_size_gb: round_1dp(total_size_gb),
                    total_space_saved_gb: round_1dp(saved_gb),
                })
            }
            BatchKey::Movie { movie_id } => {
                let row = sqlx::query!(
                    r#"
                    SELECT m.title, m.rating,
                           COUNT(*)::bigint AS file_count,
                           SUM(mf.size_bytes)::bigint AS total_size_bytes,
                           SUM(mf.size_bytes * LEAST(GREATEST(COALESCE((mf.transcode_spec->>'compression_potential')::float8, 0), 0), 1))::bigint AS saved_bytes
                    FROM media_files mf
                    JOIN movies m ON mf.movie_id = m.id
                    WHERE mf.movie_id = $1
                      AND mf.workflow_state = 'pending_approval'
                    GROUP BY m.title, m.rating
                    "#,
                    movie_id.as_uuid(),
                )
                .fetch_one(&self.pool)
                .await?;

                let total_size_gb = bytes_to_gb(row.total_size_bytes.unwrap_or(0));
                let saved_gb = bytes_to_gb(row.saved_bytes.unwrap_or(0));

                Ok(BatchApprovalInfo {
                    title: row.title,
                    tmdb_rating: row.rating,
                    file_count: row.file_count.unwrap_or(0),
                    total_size_gb: round_1dp(total_size_gb),
                    total_space_saved_gb: round_1dp(saved_gb),
                })
            }
        }
    }
}

pub struct BatchApprovalInfo {
    pub title: String,
    pub tmdb_rating: Option<f32>,
    pub file_count: i64,
    pub total_size_gb: f64,
    pub total_space_saved_gb: f64,
}

fn bytes_to_gb(bytes: i64) -> f64 {
    bytes as f64 / 1_073_741_824.0
}

fn round_1dp(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}
