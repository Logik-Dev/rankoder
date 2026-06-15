use std::collections::HashMap;

use sqlx::PgPool;
use tracing::instrument;
use uuid::Uuid;

use crate::{
    models::{
        RetentionFileId,
        batch::BatchKey,
        common::AbsoluteFilePath,
        drafts::{EpisodeDraft, MovieDraft, SeriesDraft},
        event::MediaEvent,
        media_file::{MediaFile, MediaFileId, SizeBytes},
        movie::MovieId,
        series::SeriesId,
        transcode::TranscodeDecision,
        video::{Bitrate, VideoProperties},
        workflow::WorkflowStateTag,
    },
    store::{dto::ColorMetadataRow, dto::MediaFileRow, error::StoreError},
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

/// A `transcode_failed` event joined with the human-readable title of the media
/// it belongs to, for surfacing failures to the operator.
#[derive(Debug)]
pub struct FailureRecord {
    pub event_id: i64,
    pub media_file_id: MediaFileId,
    pub title: Option<String>,
    pub kind: String,
    pub error: String,
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
                width, bitrate_kbps, framerate, duration_seconds, transcode_spec,
                workflow_state as "workflow_state: WorkflowStateTag"
                FROM media_files
                WHERE id = $1
            "#,
            media_file_id.as_uuid(),
        )
        .fetch_one(&self.pool)
        .await?;

        let color_row = sqlx::query_as!(
            ColorMetadataRow,
            r#"
                SELECT color_primaries, color_trc, colorspace, master_display, max_cll
                FROM video_color_metadata
                WHERE media_file_id = $1
            "#,
            media_file_id.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await?;

        (row, color_row).try_into()
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

        if let Some(cm) = &video_properties.color_metadata {
            sqlx::query!(
                r#"
                    INSERT INTO video_color_metadata
                        (media_file_id, color_primaries, color_trc, colorspace, master_display, max_cll)
                    VALUES ($1, $2, $3, $4, $5, $6)
                    ON CONFLICT (media_file_id) DO UPDATE SET
                        color_primaries = EXCLUDED.color_primaries,
                        color_trc = EXCLUDED.color_trc,
                        colorspace = EXCLUDED.colorspace,
                        master_display = EXCLUDED.master_display,
                        max_cll = EXCLUDED.max_cll
                "#,
                media_file_id.as_uuid(),
                cm.color_primaries.as_str(),
                cm.color_trc.as_str(),
                cm.colorspace.as_str(),
                cm.master_display.as_deref(),
                cm.max_cll.as_deref(),
            )
            .execute(&mut *tx)
            .await?;
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

    /// TMDB id of the movie a media file belongs to, used to ask the downstream
    /// media manager (Radarr) to rescan it after transcoding. Returns `None`
    /// when the file is an episode (Sonarr's responsibility) or the movie has
    /// no TMDB id — in both cases there is nothing to ask Radarr to refresh.
    pub async fn tmdb_id_for_movie_file(
        &self,
        media_file_id: &MediaFileId,
    ) -> Result<Option<i32>, StoreError> {
        let row = sqlx::query!(
            r#"
                SELECT m.tmdb_id
                FROM media_files mf
                JOIN movies m ON mf.movie_id = m.id
                WHERE mf.id = $1
            "#,
            media_file_id.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| r.tmdb_id))
    }

    /// TVDB id of the series an episode file belongs to, used to ask Sonarr to
    /// rescan it after transcoding. Returns `None` when the file is a movie
    /// (Radarr's responsibility) or the series has no TVDB id.
    pub async fn tvdb_id_for_episode_file(
        &self,
        media_file_id: &MediaFileId,
    ) -> Result<Option<i32>, StoreError> {
        let row = sqlx::query!(
            r#"
                SELECT s.tvdb_id
                FROM media_files mf
                JOIN episodes e ON mf.episode_id = e.id
                JOIN series   s ON e.series_id   = s.id
                WHERE mf.id = $1
            "#,
            media_file_id.as_uuid(),
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.and_then(|r| r.tvdb_id))
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

    /// Apply a workflow event by deriving the next state from the state machine.
    /// Fails with `StoreError::InvalidTransition` if the event is not valid from
    /// the given `from` state.
    pub async fn apply_event(
        &self,
        media_file_id: &MediaFileId,
        from: WorkflowStateTag,
        event: &MediaEvent,
    ) -> Result<(), StoreError> {
        let to = from
            .next_on(event)
            .ok_or(StoreError::InvalidTransition { from })?;
        self.transition(media_file_id, from, to, event).await
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

    pub async fn count_in_flight_batches(&self) -> Result<i64, StoreError> {
        let row = sqlx::query!(
            r#"
            SELECT (
                (SELECT COUNT(DISTINCT (e.series_id, e.season_number))
                 FROM media_files mf JOIN episodes e ON mf.episode_id = e.id
                 WHERE mf.workflow_state IN ('pending_approval', 'transcoding'))
              + (SELECT COUNT(*) FROM media_files
                 WHERE movie_id IS NOT NULL AND workflow_state IN ('pending_approval', 'transcoding'))
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
    /// Count of media files per workflow state, for the status snapshot. States
    /// with no files are simply absent from the result.
    pub async fn fetch_state_counts(&self) -> Result<Vec<(WorkflowStateTag, i64)>, StoreError> {
        let rows = sqlx::query!(
            r#"SELECT workflow_state AS "state: WorkflowStateTag", COUNT(*) AS "count!"
               FROM media_files GROUP BY workflow_state"#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| (r.state, r.count)).collect())
    }

    /// Total bytes saved across all completed transcodes, summed from the
    /// `transcoded` events (which survive retention reaping, unlike the
    /// originals themselves).
    pub async fn fetch_total_space_saved_bytes(&self) -> Result<i64, StoreError> {
        let row = sqlx::query!(
            r#"
            SELECT COALESCE(
                       SUM((event->>'original_size')::bigint - (event->>'new_size')::bigint),
                       0
                   )::bigint AS "saved!"
            FROM events
            WHERE event->>'type' = 'transcoded'
            "#,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(row.saved)
    }

    /// The highest event id currently present, used to seed the failure
    /// high-water mark so a restart doesn't replay historical failures.
    pub async fn fetch_max_event_id(&self) -> Result<i64, StoreError> {
        let row = sqlx::query!(r#"SELECT COALESCE(MAX(id), 0) AS "max!" FROM events"#)
            .fetch_one(&self.pool)
            .await?;

        Ok(row.max)
    }

    /// New `transcode_failed` events with id greater than `after_id`, oldest
    /// first, joined with the owning movie/series title.
    pub async fn fetch_failures_after(
        &self,
        after_id: i64,
    ) -> Result<Vec<FailureRecord>, StoreError> {
        let rows = sqlx::query!(
            r#"
            SELECT e.id                                            AS "event_id!",
                   e.media_file_id                                 AS "media_file_id!",
                   e.event->>'error'                               AS error,
                   COALESCE(m.title, s.title)                      AS title,
                   CASE WHEN mf.movie_id IS NOT NULL
                        THEN 'movie' ELSE 'episode' END            AS "kind!"
            FROM events e
            JOIN media_files mf ON mf.id = e.media_file_id
            LEFT JOIN movies m   ON mf.movie_id = m.id
            LEFT JOIN episodes ep ON mf.episode_id = ep.id
            LEFT JOIN series s    ON ep.series_id = s.id
            WHERE e.id > $1 AND e.event->>'type' = 'transcode_failed'
            ORDER BY e.id
            "#,
            after_id,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| FailureRecord {
                event_id: r.event_id,
                media_file_id: MediaFileId::from(r.media_file_id),
                title: r.title,
                kind: r.kind,
                error: r.error.unwrap_or_default(),
            })
            .collect())
    }

    /// The most recent `transcode_failed` event, for the status snapshot.
    pub async fn fetch_last_failure(&self) -> Result<Option<FailureRecord>, StoreError> {
        let row = sqlx::query!(
            r#"
            SELECT e.id                                            AS "event_id!",
                   e.media_file_id                                 AS "media_file_id!",
                   e.event->>'error'                               AS error,
                   COALESCE(m.title, s.title)                      AS title,
                   CASE WHEN mf.movie_id IS NOT NULL
                        THEN 'movie' ELSE 'episode' END            AS "kind!"
            FROM events e
            JOIN media_files mf ON mf.id = e.media_file_id
            LEFT JOIN movies m   ON mf.movie_id = m.id
            LEFT JOIN episodes ep ON mf.episode_id = ep.id
            LEFT JOIN series s    ON ep.series_id = s.id
            WHERE e.event->>'type' = 'transcode_failed'
            ORDER BY e.id DESC
            LIMIT 1
            "#,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| FailureRecord {
            event_id: r.event_id,
            media_file_id: MediaFileId::from(r.media_file_id),
            title: r.title,
            kind: r.kind,
            error: r.error.unwrap_or_default(),
        }))
    }

    pub async fn fetch_files_in_state(
        &self,
        state: WorkflowStateTag,
    ) -> Result<Vec<MediaFileId>, StoreError> {
        let rows = sqlx::query!(
            r#"SELECT id FROM media_files WHERE workflow_state = $1"#,
            state as WorkflowStateTag,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| MediaFileId::from(r.id)).collect())
    }

    #[instrument(skip(self))]
    pub async fn complete_transcode(
        &self,
        media_file_id: &MediaFileId,
        new_path: &AbsoluteFilePath,
        new_size: SizeBytes,
        new_bitrate: Option<&Bitrate>,
        original_size: SizeBytes,
        retention_path: &str,
    ) -> Result<(), StoreError> {
        let new_path_str = new_path.as_ref().to_str().ok_or_else(|| {
            StoreError::Domain(crate::models::error::DomainError::InvalidPath(
                "<non-UTF-8>".into(),
            ))
        })?;

        let mut tx = self.pool.begin().await?;

        let result = sqlx::query!(
            r#"
                UPDATE media_files
                SET file_path = $1, size_bytes = $2, video_codec = 'hevc',
                    bitrate_kbps = $3, workflow_state = $4
                WHERE id = $5 AND workflow_state = $6
            "#,
            new_path_str,
            new_size.as_u64() as i64,
            new_bitrate.map(|b| b.as_bps() as i32),
            WorkflowStateTag::Done as WorkflowStateTag,
            media_file_id.as_uuid(),
            WorkflowStateTag::Transcoding as WorkflowStateTag,
        )
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() == 0 {
            return Err(StoreError::StaleState {
                expected: WorkflowStateTag::Transcoding,
            });
        }

        let event = serde_json::to_value(&MediaEvent::Transcoded {
            original_size: original_size.as_u64(),
            new_size: new_size.as_u64(),
        })?;

        sqlx::query!(
            r#"INSERT INTO events(media_file_id, event) VALUES($1, $2)"#,
            media_file_id.as_uuid(),
            event,
        )
        .execute(&mut *tx)
        .await?;

        sqlx::query!(
            r#"
                INSERT INTO retention_files (media_file_id, retained_path, original_size_bytes)
                VALUES ($1, $2, $3)
            "#,
            media_file_id.as_uuid(),
            retention_path,
            original_size.as_u64() as i64,
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    pub async fn fetch_expired_retention_files(
        &self,
        retention_days: i32,
    ) -> Result<Vec<(RetentionFileId, String)>, StoreError> {
        let rows = sqlx::query!(
            r#"
                SELECT id, retained_path
                FROM retention_files
                WHERE moved_at < NOW() - make_interval(days => $1)
            "#,
            retention_days,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| (RetentionFileId::from(r.id), r.retained_path))
            .collect())
    }

    pub async fn delete_retention_file(&self, id: &RetentionFileId) -> Result<(), StoreError> {
        sqlx::query!(r#"DELETE FROM retention_files WHERE id = $1"#, id.as_uuid(),)
            .execute(&self.pool)
            .await?;

        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use sqlx::PgPool;
    use uuid::Uuid;

    async fn connect_db() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        PgPool::connect(&url).await.ok()
    }

    /// Inserts a movie + one media_file in the given state. Returns the movie id
    /// (deleting it cascades to the media_file).
    async fn insert_movie_file(pool: &PgPool, state: WorkflowStateTag) -> Uuid {
        let movie_id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO movies (id, title) VALUES ($1, 'in-flight test')",
            movie_id,
        )
        .execute(pool)
        .await
        .unwrap();

        let file_id = Uuid::now_v7();
        sqlx::query!(
            r#"INSERT INTO media_files (id, movie_id, file_path, workflow_state)
               VALUES ($1, $2, $3, $4)"#,
            file_id,
            movie_id,
            format!("/tmp/inflight_{file_id}.mkv"),
            state as WorkflowStateTag,
        )
        .execute(pool)
        .await
        .unwrap();

        movie_id
    }

    /// Inserts a series + a single season of `episodes` episodes, each with one
    /// media_file in the given state. Returns the series id (cascade-deletes).
    async fn insert_season_files(pool: &PgPool, state: WorkflowStateTag, episodes: i16) -> Uuid {
        let series_id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO series (id, title) VALUES ($1, 'in-flight series')",
            series_id,
        )
        .execute(pool)
        .await
        .unwrap();

        for ep in 1..=episodes {
            let ep_id = Uuid::now_v7();
            sqlx::query!(
                r#"INSERT INTO episodes (id, series_id, season_number, episode_number, title)
                   VALUES ($1, $2, 1, $3, 'ep')"#,
                ep_id,
                series_id,
                ep,
            )
            .execute(pool)
            .await
            .unwrap();

            let file_id = Uuid::now_v7();
            sqlx::query!(
                r#"INSERT INTO media_files (id, episode_id, file_path, workflow_state)
                   VALUES ($1, $2, $3, $4)"#,
                file_id,
                ep_id,
                format!("/tmp/inflight_ep_{file_id}.mkv"),
                state as WorkflowStateTag,
            )
            .execute(pool)
            .await
            .unwrap();
        }

        series_id
    }

    #[tokio::test]
    #[serial]
    async fn count_in_flight_includes_transcoding_and_pending_not_terminal() {
        let Some(pool) = connect_db().await else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let store = MediaStore::new(pool.clone());

        // Delta against a baseline so any pre-existing rows cancel out; #[serial]
        // guarantees no concurrent churn of in-flight states during the test.
        let baseline = store.count_in_flight_batches().await.unwrap();

        let m_transcoding = insert_movie_file(&pool, WorkflowStateTag::Transcoding).await;
        let m_pending = insert_movie_file(&pool, WorkflowStateTag::PendingApproval).await;
        let season = insert_season_files(&pool, WorkflowStateTag::Transcoding, 2).await;

        // Non in-flight: must not be counted.
        let m_analyzed = insert_movie_file(&pool, WorkflowStateTag::Analyzed).await;
        let m_done = insert_movie_file(&pool, WorkflowStateTag::Done).await;

        let after = store.count_in_flight_batches().await.unwrap();
        assert_eq!(
            after - baseline,
            3,
            "2 in-flight movies + 1 in-flight season (multi-episode = one batch); \
             analyzed/done excluded"
        );

        for id in [m_transcoding, m_pending, m_analyzed, m_done] {
            sqlx::query!("DELETE FROM movies WHERE id = $1", id)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query!("DELETE FROM series WHERE id = $1", season)
            .execute(&pool)
            .await
            .unwrap();
    }

    fn failed_count(counts: &[(WorkflowStateTag, i64)]) -> i64 {
        counts
            .iter()
            .find(|(s, _)| *s == WorkflowStateTag::Failed)
            .map(|(_, c)| *c)
            .unwrap_or(0)
    }

    async fn insert_movie_titled(pool: &PgPool, title: &str, state: WorkflowStateTag) -> (Uuid, Uuid) {
        let movie_id = Uuid::now_v7();
        sqlx::query!("INSERT INTO movies (id, title) VALUES ($1, $2)", movie_id, title)
            .execute(pool)
            .await
            .unwrap();

        let file_id = Uuid::now_v7();
        sqlx::query!(
            r#"INSERT INTO media_files (id, movie_id, file_path, workflow_state)
               VALUES ($1, $2, $3, $4)"#,
            file_id,
            movie_id,
            format!("/tmp/status_{file_id}.mkv"),
            state as WorkflowStateTag,
        )
        .execute(pool)
        .await
        .unwrap();

        (movie_id, file_id)
    }

    #[tokio::test]
    #[serial]
    async fn status_queries_surface_failures_and_savings() {
        let Some(pool) = connect_db().await else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let store = MediaStore::new(pool.clone());

        let max_before = store.fetch_max_event_id().await.unwrap();
        let saved_before = store.fetch_total_space_saved_bytes().await.unwrap();
        let failed_before = failed_count(&store.fetch_state_counts().await.unwrap());

        // A failed movie with a transcode_failed event.
        let (fail_movie, fail_file) =
            insert_movie_titled(&pool, "Inception fail", WorkflowStateTag::Failed).await;
        sqlx::query!(
            r#"INSERT INTO events (media_file_id, event) VALUES ($1, $2)"#,
            fail_file,
            serde_json::json!({ "type": "transcode_failed", "error": "ffmpeg boom" }),
        )
        .execute(&pool)
        .await
        .unwrap();

        // A completed movie whose transcoded event records a 3 GB-ish saving.
        let (done_movie, done_file) =
            insert_movie_titled(&pool, "Saver", WorkflowStateTag::Done).await;
        sqlx::query!(
            r#"INSERT INTO events (media_file_id, event) VALUES ($1, $2)"#,
            done_file,
            serde_json::json!({ "type": "transcoded", "original_size": 5_000_000, "new_size": 2_000_000 }),
        )
        .execute(&pool)
        .await
        .unwrap();

        // New failures since the high-water mark include ours, with title + kind.
        let new_failures = store.fetch_failures_after(max_before).await.unwrap();
        let ours = new_failures
            .iter()
            .find(|f| f.media_file_id.as_uuid() == fail_file)
            .expect("our failure should be returned");
        assert_eq!(ours.kind, "movie");
        assert_eq!(ours.title.as_deref(), Some("Inception fail"));
        assert_eq!(ours.error, "ffmpeg boom");

        // Most recent failure is the one we just inserted (#[serial]).
        let last = store.fetch_last_failure().await.unwrap().unwrap();
        assert_eq!(last.media_file_id.as_uuid(), fail_file);

        // Savings delta isolates our transcoded event.
        let saved_after = store.fetch_total_space_saved_bytes().await.unwrap();
        assert_eq!(saved_after - saved_before, 3_000_000);

        // The failed count grew by exactly one.
        let failed_after = failed_count(&store.fetch_state_counts().await.unwrap());
        assert_eq!(failed_after - failed_before, 1);

        for id in [fail_movie, done_movie] {
            sqlx::query!("DELETE FROM movies WHERE id = $1", id)
                .execute(&pool)
                .await
                .unwrap();
        }
    }
}
