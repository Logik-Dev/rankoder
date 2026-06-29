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

/// One `(codec, state)` cell of the dashboard breakdown: how many files and how
/// many bytes sit in each codec/workflow-state combination. Lets the operator
/// see *why* the saved figure is what it is (e.g. all `done` is HEVC→HEVC while
/// the h264 backlog waits in `analyzed`/`pending_approval`).
#[derive(Debug)]
pub struct CodecStateBreakdown {
    pub codec: String,
    pub state: WorkflowStateTag,
    pub count: i64,
    pub total_bytes: i64,
}

/// Work that is decided but not yet realised: files in `analyzed`,
/// `pending_approval` or `transcoding`. `projected_saved_bytes` uses the
/// `estimated_saving_ratio` already stored at analysis time, so the dashboard
/// can reframe "space saved so far" against "space still to gain".
#[derive(Debug, Default)]
pub struct Backlog {
    pub file_count: i64,
    pub total_bytes: i64,
    pub projected_saved_bytes: i64,
}

/// Originals still held in retention after a successful transcode, split by
/// whether the encode's quality is confirmed (`done` + VMAF ≥ `min_vmaf`).
/// `confirmed_*` are safe to delete; `held_*` are kept until verified.
#[derive(Debug, Default)]
pub struct RetentionSummary {
    pub confirmed_count: i64,
    pub confirmed_bytes: i64,
    pub held_count: i64,
    pub held_bytes: i64,
}

/// The quality-rejected population: files left in `skipped` because their
/// post-encode VMAF was below `MIN_VMAF`. Identified by carrying a recorded VMAF
/// (the only `skipped` files that do — analysis-stage skips never encode, and
/// the size-reduction skip returns before VMAF is measured). `total_bytes` is
/// the originals' size, i.e. the space a successful re-verify would reclaim.
#[derive(Debug, Default)]
pub struct QualitySkipSummary {
    pub count: i64,
    pub total_bytes: i64,
}

/// Coarse cause of a transcode failure. Drives the failure panel and, later,
/// scopes a class-aware requeue: some classes (swap I/O errors) are
/// environmental and need a host fix first, so requeuing them blindly just
/// burns an encode and re-fails — only `auto_requeueable` classes are safe to
/// re-drive on their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureClass {
    /// `MissingVideoProperties`: ffprobe data was never (or no longer) present.
    /// Re-driving through `discovered` re-probes and repopulates it.
    MissingVideoProperties,
    /// `rename(2)` denied — directory not writable by the service user.
    SwapPermission,
    /// Target filesystem mounted read-only (sandbox or mount issue).
    SwapReadOnly,
    /// `rename(2)` across filesystems (e.g. mergerfs branches).
    SwapCrossDevice,
    /// The encode itself failed (bad source, unsupported feature, …).
    Ffmpeg,
    /// Anything not matched above.
    Other,
}

impl FailureClass {
    /// Map a failure `error` string to its class via substring match. The
    /// substrings are the stable parts of the messages emitted on the transcode
    /// path (`swap failed: <io::Error>`, `MissingVideoProperties`, etc.).
    pub fn classify(error: &str) -> Self {
        if error.contains("video properties missing") {
            Self::MissingVideoProperties
        } else if error.contains("Permission denied") {
            Self::SwapPermission
        } else if error.contains("Read-only file system") {
            Self::SwapReadOnly
        } else if error.contains("cross-device") {
            Self::SwapCrossDevice
        } else if error.contains("ffmpeg failed") {
            Self::Ffmpeg
        } else {
            Self::Other
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::MissingVideoProperties => "missing video properties",
            Self::SwapPermission => "swap: permission denied",
            Self::SwapReadOnly => "swap: read-only filesystem",
            Self::SwapCrossDevice => "swap: cross-device link",
            Self::Ffmpeg => "ffmpeg encode failed",
            Self::Other => "other",
        }
    }

    /// Whether a plain requeue can resolve it. Swap I/O classes are
    /// environmental: they need a host/config fix before re-driving, otherwise
    /// the file re-encodes only to fail again at the swap.
    pub fn auto_requeueable(self) -> bool {
        matches!(self, Self::MissingVideoProperties)
    }

    /// Stable machine key for form values / round-tripping over HTTP (distinct
    /// from the human `label`, which may contain spaces and punctuation).
    pub fn key(self) -> &'static str {
        match self {
            Self::MissingVideoProperties => "missing_video_properties",
            Self::SwapPermission => "swap_permission",
            Self::SwapReadOnly => "swap_read_only",
            Self::SwapCrossDevice => "swap_cross_device",
            Self::Ffmpeg => "ffmpeg",
            Self::Other => "other",
        }
    }

    /// Inverse of [`key`]; `None` for an unknown key.
    pub fn from_key(key: &str) -> Option<Self> {
        Some(match key {
            "missing_video_properties" => Self::MissingVideoProperties,
            "swap_permission" => Self::SwapPermission,
            "swap_read_only" => Self::SwapReadOnly,
            "swap_cross_device" => Self::SwapCrossDevice,
            "ffmpeg" => Self::Ffmpeg,
            "other" => Self::Other,
            _ => return None,
        })
    }
}

/// One row of the failure panel: a cause and how many currently-`failed` files
/// carry it (counted from each file's most recent `transcode_failed` event).
#[derive(Debug)]
pub struct FailureBreakdownRow {
    pub class: FailureClass,
    pub count: i64,
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
                width, bitrate_kbps, framerate, duration_seconds, dv_profile, transcode_spec,
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
                    bitrate_kbps = $5, framerate = $6, duration_seconds = $7,
                    dv_profile = $8, workflow_state = $9
                WHERE id = $10 AND workflow_state = $11
            "#,
            video_properties.size_bytes.as_u64() as i64,
            video_properties.video_codec.as_ref(),
            video_properties.resolution.height() as i32,
            video_properties.resolution.width() as i32,
            video_properties.bitrate.as_ref().map(|b| b.as_bps() as i32),
            video_properties.framerate.as_ref().map(|f| f.to_string()),
            video_properties.duration.as_ref().map(|d| d.as_secs_f64()),
            video_properties.dv_profile.map(|p| p as i16),
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
        // Only resume work for files still on disk. A 'missing' file (its
        // provider item vanished, see reconcile_missing_files) would just fail
        // to probe, so the catch-up skips it.
        let rows = sqlx::query!(
            r#"SELECT id FROM media_files
               WHERE workflow_state NOT IN ('done', 'skipped', 'failed')
                 AND file_status = 'present'"#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| MediaFileId::from(r.id)).collect())
    }

    /// Mark as `missing` every still-`present` file the provider stopped
    /// listing. A sync only upserts the items it gets back (bumping
    /// `last_seen_at`), so any `present` row untouched since `cutoff` (captured
    /// just before the sync started) is an item that disappeared — typically a
    /// file moved/renamed on disk, which the provider re-imports under a new id,
    /// orphaning the old row. Reversible: if the item reappears, the upsert
    /// flips it back to `present`. Returns the number of rows newly marked.
    pub async fn reconcile_missing_files(
        &self,
        cutoff: time::OffsetDateTime,
    ) -> Result<u64, StoreError> {
        let result = sqlx::query!(
            r#"UPDATE media_files
               SET file_status = 'missing'
               WHERE file_status = 'present' AND last_seen_at < $1"#,
            cutoff,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
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
                estimated_saving_ratio,
            } => {
                let mut tx = self.pool.begin().await?;
                let spec = serde_json::json!({
                    "crf": crf,
                    "bpp": bpp,
                    "compression_potential": compression_potential,
                    "estimated_saving_ratio": estimated_saving_ratio,
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
                           SUM(mf.size_bytes * GREATEST(LEAST(COALESCE((mf.transcode_spec->>'estimated_saving_ratio')::float8, 0), 1), 0))::bigint AS saved_bytes
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
                           SUM(mf.size_bytes * GREATEST(LEAST(COALESCE((mf.transcode_spec->>'estimated_saving_ratio')::float8, 0), 1), 0))::bigint AS saved_bytes
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

    /// All batches currently in `pending_approval`, paired with their request
    /// info (title, file count, sizes, rating) for the operator UI. Same shape as
    /// [`Self::fetch_stale_pending_batches`] minus the staleness threshold —
    /// oldest first — then resolves each key through
    /// [`Self::fetch_batch_request_info`] so the UI shows exactly what the MQTT
    /// request carries.
    pub async fn fetch_pending_batches(
        &self,
    ) -> Result<Vec<(BatchKey, BatchApprovalInfo)>, StoreError> {
        let rows = sqlx::query!(
            r#"
            SELECT 'season' AS kind, e.series_id, e.season_number AS season, NULL::uuid AS movie_id,
                   MIN(mf.updated_at) AS oldest
            FROM media_files mf JOIN episodes e ON mf.episode_id = e.id
            WHERE mf.workflow_state = 'pending_approval'
            GROUP BY e.series_id, e.season_number
            UNION ALL
            SELECT 'movie', NULL::uuid, NULL::smallint, mf.movie_id, mf.updated_at
            FROM media_files mf
            WHERE mf.movie_id IS NOT NULL
              AND mf.workflow_state = 'pending_approval'
            ORDER BY oldest ASC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let keys: Vec<BatchKey> = rows
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
            .collect();

        let mut batches = Vec::with_capacity(keys.len());
        for key in keys {
            let info = self.fetch_batch_request_info(&key).await?;
            batches.push((key, info));
        }
        Ok(batches)
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

    /// Files and bytes per `(codec, state)`, ordered by codec then descending
    /// count. Files with no probed codec yet collapse into `(unknown)`.
    pub async fn fetch_codec_state_breakdown(
        &self,
    ) -> Result<Vec<CodecStateBreakdown>, StoreError> {
        let rows = sqlx::query!(
            r#"
            SELECT COALESCE(video_codec, '(unknown)') AS "codec!",
                   workflow_state AS "state: WorkflowStateTag",
                   COUNT(*) AS "count!",
                   COALESCE(SUM(size_bytes), 0)::bigint AS "total_bytes!"
            FROM media_files
            GROUP BY 1, 2
            ORDER BY 1, 3 DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| CodecStateBreakdown {
                codec: r.codec,
                state: r.state,
                count: r.count,
                total_bytes: r.total_bytes,
            })
            .collect())
    }

    /// Decided-but-not-realised work (`analyzed` + `pending_approval` +
    /// `transcoding`): file count, total bytes and projected savings from the
    /// stored `estimated_saving_ratio` (clamped to [0, 1], same as the approval
    /// estimate).
    pub async fn fetch_backlog(&self) -> Result<Backlog, StoreError> {
        let row = sqlx::query!(
            r#"
            SELECT COUNT(*) AS "file_count!",
                   COALESCE(SUM(size_bytes), 0)::bigint AS "total_bytes!",
                   COALESCE(SUM(
                       size_bytes
                       * GREATEST(LEAST(COALESCE((transcode_spec->>'estimated_saving_ratio')::float8, 0), 1), 0)
                   ), 0)::bigint AS "projected_saved_bytes!"
            FROM media_files
            WHERE workflow_state IN ('analyzed', 'pending_approval', 'transcoding')
            "#,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(Backlog {
            file_count: row.file_count,
            total_bytes: row.total_bytes,
            projected_saved_bytes: row.projected_saved_bytes,
        })
    }

    /// Distribution of recorded VMAF scores, rounded to the nearest integer, as
    /// `(score, count)` pairs ordered by score. Drives the dashboard histogram;
    /// only files with a measured score (`transcode_spec.vmaf`) are included.
    pub async fn fetch_vmaf_distribution(&self) -> Result<Vec<(i32, i64)>, StoreError> {
        let rows = sqlx::query!(
            r#"
            SELECT round((transcode_spec->>'vmaf')::float8)::int AS "vmaf!",
                   COUNT(*) AS "count!"
            FROM media_files
            WHERE jsonb_exists(transcode_spec, 'vmaf')
            GROUP BY 1 ORDER BY 1
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| (r.vmaf, r.count)).collect())
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

    /// Failure causes across the currently-`failed` files, grouped by class.
    /// Takes each file's most recent `transcode_failed` event (a file may have
    /// several from retries) and folds the errors into [`FailureClass`] counts,
    /// ordered by descending count. Read-only; drives the dashboard panel.
    pub async fn fetch_failure_breakdown(&self) -> Result<Vec<FailureBreakdownRow>, StoreError> {
        let rows = sqlx::query!(
            r#"
            SELECT DISTINCT ON (mf.id) e.event->>'error' AS error
            FROM media_files mf
            JOIN events e
              ON e.media_file_id = mf.id
             AND e.event->>'type' = 'transcode_failed'
            WHERE mf.workflow_state = 'failed'
            ORDER BY mf.id, e.id DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let mut counts: HashMap<FailureClass, i64> = HashMap::new();
        for r in rows {
            let class = FailureClass::classify(r.error.as_deref().unwrap_or(""));
            *counts.entry(class).or_insert(0) += 1;
        }

        let mut breakdown: Vec<FailureBreakdownRow> = counts
            .into_iter()
            .map(|(class, count)| FailureBreakdownRow { class, count })
            .collect();
        breakdown.sort_by_key(|b| std::cmp::Reverse(b.count));

        Ok(breakdown)
    }

    /// Requeue the currently-`failed` files of a given [`FailureClass`] back to
    /// `discovered`, so the event pipeline re-probes them from scratch. Returns
    /// the ids actually moved.
    ///
    /// Classification reuses `FailureClass::classify` on each file's most recent
    /// failure error (single source of truth, same as the panel). Each file is
    /// moved via [`transition`] with a `from = Failed` guard, so a file already
    /// re-driven concurrently (e.g. by a double-submit) is simply skipped —
    /// idempotent.
    pub async fn requeue_failed(
        &self,
        class: FailureClass,
    ) -> Result<Vec<MediaFileId>, StoreError> {
        let rows = sqlx::query!(
            r#"
            SELECT DISTINCT ON (mf.id) mf.id, e.event->>'error' AS error
            FROM media_files mf
            JOIN events e
              ON e.media_file_id = mf.id
             AND e.event->>'type' = 'transcode_failed'
            WHERE mf.workflow_state = 'failed'
            ORDER BY mf.id, e.id DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        // The event only serves to wake the pipeline (the listener keys off the
        // file id; the orchestrator dispatches on the file's state). Tag the
        // source so a requeue is distinguishable from the original sync.
        let event = MediaEvent::Discovered {
            source: "requeue".into(),
        };

        let mut moved = Vec::new();
        for r in rows {
            if FailureClass::classify(r.error.as_deref().unwrap_or("")) != class {
                continue;
            }
            let id = MediaFileId::from(r.id);
            match self
                .transition(
                    &id,
                    WorkflowStateTag::Failed,
                    WorkflowStateTag::Discovered,
                    &event,
                )
                .await
            {
                Ok(()) => moved.push(id),
                // Lost the race (already moved) — skip, stay idempotent.
                Err(StoreError::StaleState { .. }) => {}
                Err(e) => return Err(e),
            }
        }

        Ok(moved)
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

    /// Merge the measured VMAF into `transcode_spec` so it's queryable for
    /// every transcode attempt (accepted or rejected), independent of the
    /// workflow state the row ends up in. Best-effort metadata, no state guard.
    pub async fn record_vmaf(
        &self,
        media_file_id: &MediaFileId,
        vmaf: f64,
    ) -> Result<(), StoreError> {
        sqlx::query!(
            r#"
                UPDATE media_files
                SET transcode_spec = COALESCE(transcode_spec, '{}'::jsonb)
                    || jsonb_build_object('vmaf', $2::float8)
                WHERE id = $1
            "#,
            media_file_id.as_uuid(),
            vmaf,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// `done` files that predate the quality gate and still have their original
    /// in retention (the `JOIN` guarantees the source is on disk — the reaper
    /// deletes the file and the row together). Returns `(id, original_path,
    /// current_path)` for a retroactive VMAF measurement. Idempotent: scored
    /// files fall out of the filter.
    pub async fn fetch_done_files_missing_vmaf(
        &self,
    ) -> Result<Vec<(MediaFileId, String, String)>, StoreError> {
        let rows = sqlx::query!(
            r#"
                SELECT mf.id, rf.retained_path, mf.file_path
                FROM media_files mf
                JOIN retention_files rf ON rf.media_file_id = mf.id
                WHERE mf.workflow_state = 'done'
                  AND COALESCE(jsonb_exists(mf.transcode_spec, 'vmaf'), false) = false
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| (MediaFileId::from(r.id), r.retained_path, r.file_path))
            .collect())
    }

    /// Move quality-rejected files (`skipped` with a recorded VMAF, i.e. a
    /// post-encode `QualityTooLow`) back into `transcoding` so they are
    /// re-encoded against the current threshold. Only files whose previously
    /// measured score now clears `min_vmaf` are touched, which keeps the
    /// operation safe and idempotent — a re-encode reproduces ~the same score,
    /// so genuine rejects are never looped. Returns the requeued ids; the caller
    /// feeds them to the transcode channel.
    pub async fn requeue_quality_skips(
        &self,
        min_vmaf: f64,
    ) -> Result<Vec<MediaFileId>, StoreError> {
        let rows = sqlx::query!(
            r#"
                UPDATE media_files
                SET workflow_state = 'transcoding'
                WHERE workflow_state = 'skipped'
                  AND jsonb_exists(transcode_spec, 'vmaf')
                  AND (transcode_spec->>'vmaf')::float8 >= $1
                RETURNING id
            "#,
            min_vmaf,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| MediaFileId::from(r.id)).collect())
    }

    /// Count and original size of the quality-rejected population (`skipped`
    /// files carrying a recorded VMAF, i.e. a post-encode `QualityTooLow`).
    /// Drives the "Quality skips" panel; pairs with [`Self::recheck_quality_skips`].
    pub async fn fetch_quality_skip_summary(&self) -> Result<QualitySkipSummary, StoreError> {
        let row = sqlx::query!(
            r#"
                SELECT COUNT(*) AS "count!",
                       COALESCE(SUM(size_bytes), 0)::bigint AS "total_bytes!"
                FROM media_files
                WHERE workflow_state = 'skipped'
                  AND jsonb_exists(transcode_spec, 'vmaf')
            "#,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(QualitySkipSummary {
            count: row.count,
            total_bytes: row.total_bytes,
        })
    }

    /// Re-verify quality-rejected files: flip every `skipped` file with a
    /// recorded VMAF (a post-encode `QualityTooLow`) back to `transcoding`,
    /// **without** consulting the stored score. The encode and the VMAF are then
    /// recomputed from scratch and re-gated against the current `MIN_VMAF`, so a
    /// score that was a measurement artefact (e.g. the framesync misalignment bug)
    /// is corrected and the file kept if it now clears the bar — genuine rejects
    /// simply re-skip. Distinct from [`Self::requeue_quality_skips`], which trusts
    /// the stored score and is for re-driving after *lowering* the threshold.
    /// Returns the requeued ids; the transcode orchestrator's stale re-queue picks
    /// them up on its own.
    pub async fn recheck_quality_skips(&self) -> Result<Vec<MediaFileId>, StoreError> {
        let rows = sqlx::query!(
            r#"
                UPDATE media_files
                SET workflow_state = 'transcoding'
                WHERE workflow_state = 'skipped'
                  AND jsonb_exists(transcode_spec, 'vmaf')
                RETURNING id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| MediaFileId::from(r.id)).collect())
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

    /// Originals still held in retention, split by whether their transcode's
    /// quality is *confirmed*: the file is `done` and carries a recorded VMAF at
    /// or above `min_vmaf`. Confirmed originals are safe to delete (the encode is
    /// verified good); the rest — no VMAF yet, or below the bar — are kept until
    /// verified. Drives the retention panel.
    pub async fn fetch_retention_summary(
        &self,
        min_vmaf: f64,
    ) -> Result<RetentionSummary, StoreError> {
        let row = sqlx::query!(
            r#"
            SELECT
                COUNT(*) FILTER (WHERE confirmed)                                  AS "confirmed_count!",
                COALESCE(SUM(original_size_bytes) FILTER (WHERE confirmed), 0)::bigint     AS "confirmed_bytes!",
                COUNT(*) FILTER (WHERE NOT confirmed)                              AS "held_count!",
                COALESCE(SUM(original_size_bytes) FILTER (WHERE NOT confirmed), 0)::bigint AS "held_bytes!"
            FROM (
                SELECT rf.original_size_bytes,
                       COALESCE(
                           mf.workflow_state = 'done'
                           AND (mf.transcode_spec->>'vmaf')::float8 >= $1,
                           false
                       ) AS confirmed
                FROM retention_files rf
                JOIN media_files mf ON mf.id = rf.media_file_id
            ) t
            "#,
            min_vmaf,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(RetentionSummary {
            confirmed_count: row.confirmed_count,
            confirmed_bytes: row.confirmed_bytes,
            held_count: row.held_count,
            held_bytes: row.held_bytes,
        })
    }

    /// Retention rows whose transcode is quality-confirmed (`done` + VMAF ≥
    /// `min_vmaf`), i.e. the originals safe to delete. Returns `(id, path, size)`
    /// so the caller can reap them and report freed bytes. A NULL VMAF fails the
    /// `>=` comparison and is therefore excluded (kept).
    pub async fn fetch_confirmed_originals(
        &self,
        min_vmaf: f64,
    ) -> Result<Vec<(RetentionFileId, String, i64)>, StoreError> {
        let rows = sqlx::query!(
            r#"
            SELECT rf.id, rf.retained_path, rf.original_size_bytes
            FROM retention_files rf
            JOIN media_files mf ON mf.id = rf.media_file_id
            WHERE mf.workflow_state = 'done'
              AND (mf.transcode_spec->>'vmaf')::float8 >= $1
            "#,
            min_vmaf,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    RetentionFileId::from(r.id),
                    r.retained_path,
                    r.original_size_bytes,
                )
            })
            .collect())
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
    use sqlx::PgPool;
    use uuid::Uuid;

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

    #[sqlx::test]
    async fn count_in_flight_includes_transcoding_and_pending_not_terminal(pool: PgPool) {
        let store = MediaStore::new(pool.clone());

        insert_movie_file(&pool, WorkflowStateTag::Transcoding).await;
        insert_movie_file(&pool, WorkflowStateTag::PendingApproval).await;
        insert_season_files(&pool, WorkflowStateTag::Transcoding, 2).await;

        // Non in-flight: must not be counted.
        insert_movie_file(&pool, WorkflowStateTag::Analyzed).await;
        insert_movie_file(&pool, WorkflowStateTag::Done).await;

        assert_eq!(
            store.count_in_flight_batches().await.unwrap(),
            3,
            "2 in-flight movies + 1 in-flight season (multi-episode = one batch); \
             analyzed/done excluded"
        );
    }

    #[sqlx::test]
    async fn fetch_pending_batches_returns_only_pending_with_info(pool: PgPool) {
        let store = MediaStore::new(pool.clone());

        let (pending_movie, _) =
            insert_movie_titled(&pool, "Pending Movie", WorkflowStateTag::PendingApproval).await;
        let pending_season = insert_season_files(&pool, WorkflowStateTag::PendingApproval, 3).await;
        // Must be excluded: not yet pending.
        let (analyzed_movie, _) =
            insert_movie_titled(&pool, "Analyzed Movie", WorkflowStateTag::Analyzed).await;

        let batches = store.fetch_pending_batches().await.unwrap();

        let movie = batches
            .iter()
            .find(|(k, _)| {
                *k == BatchKey::Movie {
                    movie_id: MovieId::from(pending_movie),
                }
            })
            .expect("pending movie batch present");
        assert_eq!(movie.1.title, "Pending Movie");
        assert_eq!(movie.1.file_count, 1);

        let season = batches
            .iter()
            .find(|(k, _)| {
                *k == BatchKey::Season {
                    series_id: SeriesId::from(pending_season),
                    season: 1,
                }
            })
            .expect("pending season batch present");
        assert_eq!(
            season.1.file_count, 3,
            "all three episodes counted as one batch"
        );

        assert!(
            !batches.iter().any(|(k, _)| *k
                == BatchKey::Movie {
                    movie_id: MovieId::from(analyzed_movie)
                }),
            "analyzed movie must not be pending"
        );
    }

    fn failed_count(counts: &[(WorkflowStateTag, i64)]) -> i64 {
        counts
            .iter()
            .find(|(s, _)| *s == WorkflowStateTag::Failed)
            .map(|(_, c)| *c)
            .unwrap_or(0)
    }

    async fn insert_movie_titled(
        pool: &PgPool,
        title: &str,
        state: WorkflowStateTag,
    ) -> (Uuid, Uuid) {
        let movie_id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO movies (id, title) VALUES ($1, $2)",
            movie_id,
            title
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
            format!("/tmp/status_{file_id}.mkv"),
            state as WorkflowStateTag,
        )
        .execute(pool)
        .await
        .unwrap();

        (movie_id, file_id)
    }

    #[sqlx::test]
    async fn status_queries_surface_failures_and_savings(pool: PgPool) {
        let store = MediaStore::new(pool.clone());

        let max_before = store.fetch_max_event_id().await.unwrap();

        // A failed movie with a transcode_failed event.
        let (_, fail_file) =
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
        let (_, done_file) = insert_movie_titled(&pool, "Saver", WorkflowStateTag::Done).await;
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

        // Most recent failure is the one we just inserted (isolated DB).
        let last = store.fetch_last_failure().await.unwrap().unwrap();
        assert_eq!(last.media_file_id.as_uuid(), fail_file);

        // Only our transcoded event contributes on an isolated DB.
        assert_eq!(
            store.fetch_total_space_saved_bytes().await.unwrap(),
            3_000_000
        );

        // Exactly one failed file.
        assert_eq!(failed_count(&store.fetch_state_counts().await.unwrap()), 1);
    }

    #[sqlx::test]
    async fn requeue_failed_moves_only_matching_class(pool: PgPool) {
        let store = MediaStore::new(pool.clone());

        // Two failed files with different failure causes.
        let (_, perm_file) =
            insert_movie_titled(&pool, "perm fail", WorkflowStateTag::Failed).await;
        let (_, vp_file) = insert_movie_titled(&pool, "vp fail", WorkflowStateTag::Failed).await;
        for (file, error) in [
            (perm_file, "swap failed: Permission denied (os error 13)"),
            (vp_file, "video properties missing for media file"),
        ] {
            sqlx::query!(
                r#"INSERT INTO events (media_file_id, event) VALUES ($1, $2)"#,
                file,
                serde_json::json!({ "type": "transcode_failed", "error": error }),
            )
            .execute(&pool)
            .await
            .unwrap();
        }

        // Requeue only the permission-denied class.
        let moved = store
            .requeue_failed(FailureClass::SwapPermission)
            .await
            .unwrap();
        assert_eq!(moved, vec![MediaFileId::from(perm_file)]);

        // The permission file moved to discovered; the other class stays failed.
        let perm_state = sqlx::query_scalar!(
            r#"SELECT workflow_state AS "s: WorkflowStateTag" FROM media_files WHERE id = $1"#,
            perm_file,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let vp_state = sqlx::query_scalar!(
            r#"SELECT workflow_state AS "s: WorkflowStateTag" FROM media_files WHERE id = $1"#,
            vp_file,
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(perm_state, WorkflowStateTag::Discovered);
        assert_eq!(vp_state, WorkflowStateTag::Failed);

        // Idempotent: nothing left in that class to move.
        let again = store
            .requeue_failed(FailureClass::SwapPermission)
            .await
            .unwrap();
        assert!(again.is_empty());
    }

    #[sqlx::test]
    async fn confirmed_originals_gated_on_min_vmaf(pool: PgPool) {
        let store = MediaStore::new(pool.clone());
        let min_vmaf = 92.0;

        // done + VMAF above the bar -> confirmed (deletable).
        let (_, good_file) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Done,
            Some(serde_json::json!({ "vmaf": 95.0 })),
        )
        .await;
        insert_retention(&pool, good_file, "/tmp/orig_good.mkv").await;
        // done + VMAF below the bar -> held.
        let (_, low_file) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Done,
            Some(serde_json::json!({ "vmaf": 80.0 })),
        )
        .await;
        insert_retention(&pool, low_file, "/tmp/orig_low.mkv").await;
        // done but no VMAF recorded -> held (can't confirm).
        let (_, novmaf_file) =
            insert_movie_spec(&pool, WorkflowStateTag::Done, Some(serde_json::json!({}))).await;
        insert_retention(&pool, novmaf_file, "/tmp/orig_novmaf.mkv").await;

        // Only the above-bar original is offered for deletion.
        let confirmed = store.fetch_confirmed_originals(min_vmaf).await.unwrap();
        let paths: Vec<&str> = confirmed.iter().map(|(_, p, _)| p.as_str()).collect();
        assert!(paths.contains(&"/tmp/orig_good.mkv"));
        assert!(!paths.contains(&"/tmp/orig_low.mkv"));
        assert!(!paths.contains(&"/tmp/orig_novmaf.mkv"));

        // Summary on an isolated DB: 1 confirmed, 2 held.
        let summary = store.fetch_retention_summary(min_vmaf).await.unwrap();
        assert_eq!(summary.confirmed_count, 1);
        assert_eq!(summary.held_count, 2);
    }

    /// Inserts a movie + one media_file in `state` with the given
    /// `transcode_spec`. Returns `(movie_id, file_id)`; deleting the movie
    /// cascades to the file (and its retention rows).
    async fn insert_movie_spec(
        pool: &PgPool,
        state: WorkflowStateTag,
        spec: Option<serde_json::Value>,
    ) -> (Uuid, Uuid) {
        let movie_id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO movies (id, title) VALUES ($1, 'spec test')",
            movie_id,
        )
        .execute(pool)
        .await
        .unwrap();

        let file_id = Uuid::now_v7();
        sqlx::query!(
            r#"INSERT INTO media_files (id, movie_id, file_path, workflow_state, transcode_spec)
               VALUES ($1, $2, $3, $4, $5)"#,
            file_id,
            movie_id,
            format!("/tmp/cur_{file_id}.mkv"),
            state as WorkflowStateTag,
            spec,
        )
        .execute(pool)
        .await
        .unwrap();

        (movie_id, file_id)
    }

    async fn insert_retention(pool: &PgPool, file_id: Uuid, retained_path: &str) {
        sqlx::query!(
            r#"INSERT INTO retention_files (media_file_id, retained_path, original_size_bytes)
               VALUES ($1, $2, $3)"#,
            file_id,
            retained_path,
            1_000_000i64,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    async fn file_state(pool: &PgPool, id: Uuid) -> String {
        sqlx::query_scalar!(
            r#"SELECT workflow_state::text as "s!" FROM media_files WHERE id = $1"#,
            id,
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[sqlx::test]
    async fn requeue_quality_skips_flips_only_eligible(pool: PgPool) {
        let store = MediaStore::new(pool.clone());

        // Skipped with a recorded VMAF at/above the (lowered) threshold.
        let (_, f_ok) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Skipped,
            Some(serde_json::json!({ "crf": 24, "vmaf": 95.0 })),
        )
        .await;
        // Below the new threshold -> must stay skipped.
        let (_, f_low) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Skipped,
            Some(serde_json::json!({ "crf": 24, "vmaf": 80.0 })),
        )
        .await;
        // Skipped without a VMAF (e.g. insufficient size reduction) -> untouched.
        let (_, f_no) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Skipped,
            Some(serde_json::json!({ "crf": 24 })),
        )
        .await;
        // Already done with a high VMAF -> not a skip, untouched.
        let (_, f_done) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Done,
            Some(serde_json::json!({ "crf": 24, "vmaf": 99.0 })),
        )
        .await;

        let ids = store.requeue_quality_skips(90.0).await.unwrap();

        assert!(
            ids.iter().any(|id| id.as_uuid() == f_ok),
            "eligible file must be requeued"
        );
        for f in [f_low, f_no, f_done] {
            assert!(
                !ids.iter().any(|id| id.as_uuid() == f),
                "ineligible file must not be requeued"
            );
        }

        assert_eq!(file_state(&pool, f_ok).await, "transcoding");
        assert_eq!(file_state(&pool, f_low).await, "skipped");
        assert_eq!(file_state(&pool, f_no).await, "skipped");
        assert_eq!(file_state(&pool, f_done).await, "done");
    }

    #[sqlx::test]
    async fn recheck_quality_skips_requeues_all_scored_skips(pool: PgPool) {
        let store = MediaStore::new(pool.clone());

        // A high stored score and a low one: both are quality skips, both must be
        // requeued regardless of the score (the whole point is to re-measure).
        let (_, f_hi) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Skipped,
            Some(serde_json::json!({ "crf": 24, "vmaf": 95.0 })),
        )
        .await;
        let (_, f_lo) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Skipped,
            Some(serde_json::json!({ "crf": 24, "vmaf": 48.0 })),
        )
        .await;
        // Skipped without a VMAF (e.g. insufficient size reduction) -> untouched.
        let (_, f_no) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Skipped,
            Some(serde_json::json!({ "crf": 24 })),
        )
        .await;
        // Done with a VMAF -> not a skip, untouched.
        let (_, f_done) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Done,
            Some(serde_json::json!({ "crf": 24, "vmaf": 99.0 })),
        )
        .await;

        let ids = store.recheck_quality_skips().await.unwrap();

        for f in [f_hi, f_lo] {
            assert!(
                ids.iter().any(|id| id.as_uuid() == f),
                "every scored skip must be requeued"
            );
        }
        for f in [f_no, f_done] {
            assert!(
                !ids.iter().any(|id| id.as_uuid() == f),
                "unscored skip / done file must not be requeued"
            );
        }

        assert_eq!(file_state(&pool, f_hi).await, "transcoding");
        assert_eq!(file_state(&pool, f_lo).await, "transcoding");
        assert_eq!(file_state(&pool, f_no).await, "skipped");
        assert_eq!(file_state(&pool, f_done).await, "done");
    }

    #[sqlx::test]
    async fn fetch_done_files_missing_vmaf_filters_correctly(pool: PgPool) {
        let store = MediaStore::new(pool.clone());

        // done + retention + no vmaf -> included.
        let (_, f_inc) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Done,
            Some(serde_json::json!({ "crf": 24 })),
        )
        .await;
        insert_retention(&pool, f_inc, &format!("/tmp/orig_{f_inc}.mkv")).await;
        // done + retention + has vmaf -> excluded (already scored).
        let (_, f_scored) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Done,
            Some(serde_json::json!({ "crf": 24, "vmaf": 97.0 })),
        )
        .await;
        insert_retention(&pool, f_scored, "/tmp/orig_scored.mkv").await;
        // done, no retention row -> excluded (original already reaped).
        let (_, f_noret) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Done,
            Some(serde_json::json!({ "crf": 24 })),
        )
        .await;
        // skipped + retention -> excluded (wrong state).
        let (_, f_skip) = insert_movie_spec(
            &pool,
            WorkflowStateTag::Skipped,
            Some(serde_json::json!({ "crf": 24 })),
        )
        .await;
        insert_retention(&pool, f_skip, "/tmp/orig_skip.mkv").await;

        let rows = store.fetch_done_files_missing_vmaf().await.unwrap();

        let found = rows
            .iter()
            .find(|(id, _, _)| id.as_uuid() == f_inc)
            .expect("done file lacking vmaf with retention must be returned");
        assert_eq!(
            found.1,
            format!("/tmp/orig_{f_inc}.mkv"),
            "retained original"
        );
        assert_eq!(
            found.2,
            format!("/tmp/cur_{f_inc}.mkv"),
            "current transcoded"
        );

        for f in [f_scored, f_noret, f_skip] {
            assert!(
                !rows.iter().any(|(id, _, _)| id.as_uuid() == f),
                "filtered file must be absent"
            );
        }
    }

    /// Inserts a movie + one media_file with an explicit `last_seen_at`,
    /// returning the media_file id. file_status defaults to 'present'.
    async fn insert_movie_file_seen_at(
        pool: &PgPool,
        state: WorkflowStateTag,
        last_seen_at: time::OffsetDateTime,
    ) -> Uuid {
        let movie_id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO movies (id, title) VALUES ($1, 'reconcile test')",
            movie_id,
        )
        .execute(pool)
        .await
        .unwrap();

        let file_id = Uuid::now_v7();
        sqlx::query!(
            r#"INSERT INTO media_files (id, movie_id, file_path, workflow_state, last_seen_at)
               VALUES ($1, $2, $3, $4, $5)"#,
            file_id,
            movie_id,
            format!("/tmp/rec_{file_id}.mkv"),
            state as WorkflowStateTag,
            last_seen_at,
        )
        .execute(pool)
        .await
        .unwrap();

        file_id
    }

    async fn file_status_of(pool: &PgPool, file_id: Uuid) -> String {
        sqlx::query_scalar!(
            r#"SELECT file_status::text AS "s!" FROM media_files WHERE id = $1"#,
            file_id,
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[sqlx::test]
    async fn reconcile_flags_only_files_unseen_since_cutoff(pool: PgPool) {
        let store = MediaStore::new(pool.clone());

        // Fixed timestamps keep the assertion independent of wall-clock time.
        let day1 = time::OffsetDateTime::from_unix_timestamp(1_577_836_800).unwrap(); // 2020-01-01
        let day3 = time::OffsetDateTime::from_unix_timestamp(1_578_009_600).unwrap(); // 2020-01-03
        let cutoff = time::OffsetDateTime::from_unix_timestamp(1_577_923_200).unwrap(); // 2020-01-02

        let stale = insert_movie_file_seen_at(&pool, WorkflowStateTag::Skipped, day1).await;
        let fresh = insert_movie_file_seen_at(&pool, WorkflowStateTag::Skipped, day3).await;

        let marked = store.reconcile_missing_files(cutoff).await.unwrap();

        assert_eq!(
            marked, 1,
            "only the file unseen since the cutoff is flagged"
        );
        assert_eq!(file_status_of(&pool, stale).await, "missing");
        assert_eq!(file_status_of(&pool, fresh).await, "present");
    }

    #[sqlx::test]
    async fn catch_up_skips_missing_files(pool: PgPool) {
        let store = MediaStore::new(pool.clone());
        let now = time::OffsetDateTime::now_utc();

        let present = insert_movie_file_seen_at(&pool, WorkflowStateTag::Discovered, now).await;
        let gone = insert_movie_file_seen_at(&pool, WorkflowStateTag::Discovered, now).await;
        sqlx::query!(
            "UPDATE media_files SET file_status = 'missing' WHERE id = $1",
            gone,
        )
        .execute(&pool)
        .await
        .unwrap();

        let active = store.fetch_active_media_files().await.unwrap();

        assert_eq!(
            active,
            vec![MediaFileId::from(present)],
            "missing files are excluded from the catch-up resume set"
        );
    }
}
