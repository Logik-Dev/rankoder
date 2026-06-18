use std::{
    collections::{HashSet, VecDeque},
    path::Path,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::{
    models::{
        common::AbsoluteFilePath,
        event::MediaEvent,
        media_file::{MediaFile, MediaFileId},
        transcode::SkipReason,
        workflow::WorkflowStateTag,
    },
    providers::{MovieNotifier, SeriesNotifier},
    store::MediaStore,
    transcode::{
        encoder::Encoder,
        error::TranscodeError,
        outcome::{CompletedTranscode, TranscodeOutcome},
        recovery::{self, RecoveryAction},
        swap::{RealFileSystem, Swapper},
        validation, vmaf,
    },
};

/// How often to re-scan for files stuck in `transcoding` and re-enqueue them.
/// A transient transcode error leaves the file in that state with no in-flight
/// task; without this it would only be retried on the next process restart.
const STALE_REQUEUE_INTERVAL: Duration = Duration::from_secs(300);

/// Optional downstream media managers refreshed after a successful transcode:
/// Radarr for movies, Sonarr for series. Each is `None` when unconfigured.
#[derive(Clone, Default)]
pub struct MediaNotifiers {
    pub movie: Option<Arc<dyn MovieNotifier>>,
    pub series: Option<Arc<dyn SeriesNotifier>>,
}

pub struct TranscodeOrchestrator {
    rx: mpsc::Receiver<MediaFileId>,
    store: Arc<MediaStore>,
    encoder: Encoder,
    tmp_dir: PathBuf,
    retention_dir: PathBuf,
    min_size_reduction: f64,
    min_vmaf: f64,
    vmaf_n_subsample: u32,
    notifiers: MediaNotifiers,
}

impl TranscodeOrchestrator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rx: mpsc::Receiver<MediaFileId>,
        store: Arc<MediaStore>,
        encoder: Encoder,
        tmp_dir: PathBuf,
        retention_dir: PathBuf,
        min_size_reduction: f64,
        min_vmaf: f64,
        vmaf_n_subsample: u32,
        notifiers: MediaNotifiers,
    ) -> Self {
        Self {
            rx,
            store,
            encoder,
            tmp_dir,
            retention_dir,
            min_size_reduction,
            min_vmaf,
            vmaf_n_subsample,
            notifiers,
        }
    }

    #[instrument(skip(self), err)]
    pub async fn run(self, token: CancellationToken) -> anyhow::Result<()> {
        info!(
            tmp_dir = %self.tmp_dir.display(),
            retention_dir = %self.retention_dir.display(),
            encoder = ?self.encoder,
            min_size_reduction = %self.min_size_reduction,
            "starting transcode orchestrator",
        );

        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let mut join_set: JoinSet<MediaFileId> = JoinSet::new();

        let store = self.store;
        let encoder = self.encoder;
        let tmp_dir = self.tmp_dir;
        let retention_dir = self.retention_dir;
        let min_size_reduction = self.min_size_reduction;
        let min_vmaf = self.min_vmaf;
        let vmaf_n_subsample = self.vmaf_n_subsample;
        let notifiers = self.notifiers;
        let mut rx = self.rx;

        // Files queued for transcoding and those currently encoding. Both are
        // consulted before enqueuing (`enqueue_unique`) so the periodic
        // re-queue never schedules a file that is already pending or running.
        let mut pending: VecDeque<MediaFileId> = VecDeque::new();
        let mut inflight: HashSet<MediaFileId> = HashSet::new();

        // Safety net for stalled work: re-enqueue files left in `transcoding`
        // (e.g. after a transient error). The first tick fires immediately, so
        // this also recovers files stuck across a restart.
        let mut requeue = tokio::time::interval(STALE_REQUEUE_INTERVAL);
        requeue.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                biased;
                _ = token.cancelled() => {
                    info!("transcode cancelled, draining remaining tasks");
                    break;
                }
                Some(media_file_id) = rx.recv() => {
                    enqueue_unique(&mut pending, &inflight, media_file_id);
                }
                _ = requeue.tick() => {
                    match store.fetch_files_in_state(WorkflowStateTag::Transcoding).await {
                        Ok(ids) => {
                            let requeued = ids
                                .into_iter()
                                .filter(|id| enqueue_unique(&mut pending, &inflight, *id))
                                .count();
                            if requeued > 0 {
                                warn!(requeued, "re-enqueued stalled transcoding files");
                            }
                        }
                        Err(e) => error!(%e, "failed to scan for stalled transcoding files"),
                    }
                }
                permit = semaphore.clone().acquire_owned(), if !pending.is_empty() => {
                    let media_file_id = pending.pop_front().unwrap();
                    let permit = permit.expect("semaphore closed");
                    inflight.insert(media_file_id);
                    let s = Arc::clone(&store);
                    let enc = encoder;
                    let t = tmp_dir.clone();
                    let r = retention_dir.clone();
                    let msr = min_size_reduction;
                    let mv = min_vmaf;
                    let ns = vmaf_n_subsample;
                    let n = notifiers.clone();

                    join_set.spawn(async move {
                        // Hold the permit for the whole encode so Semaphore(1)
                        // actually serializes transcoding.
                        let _permit = permit;
                        if let Err(e) =
                            Self::process_file(s, enc, &t, &r, msr, mv, ns, n, media_file_id).await
                        {
                            error!(%e, ?media_file_id, "transcode failed");
                        }
                        media_file_id
                    });
                }
                Some(res) = join_set.join_next() => {
                    match res {
                        Ok(id) => { inflight.remove(&id); }
                        Err(e) => error!("transcode worker task panicked: {e}"),
                    }
                }
                else => break,
            }
        }

        while let Some(res) = join_set.join_next().await {
            match res {
                Ok(id) => { inflight.remove(&id); }
                Err(e) => error!("transcode worker task panicked: {e}"),
            }
        }

        info!("transcode orchestrator shut down");
        Ok(())
    }

    #[instrument(skip(encoder, tmp_dir, retention_dir, media_file), fields(id = ?media_file.id), err)]
    #[allow(clippy::too_many_arguments)]
    async fn transcode_file(
        encoder: Encoder,
        tmp_dir: &Path,
        retention_dir: &Path,
        min_size_reduction: f64,
        min_vmaf: f64,
        vmaf_n_subsample: u32,
        media_file: &MediaFile,
    ) -> Result<TranscodeOutcome, TranscodeError> {
        let media_file_id = media_file.id;

        let video_properties = media_file
            .video_properties
            .as_ref()
            .ok_or(TranscodeError::MissingVideoProperties)?;

        let crf = media_file
            .transcode_spec
            .as_ref()
            .and_then(|s| s.get("crf"))
            .and_then(|c| c.as_u64())
            .map(|c| c as u8)
            .ok_or(TranscodeError::MissingSpec)?;

        let original_size = video_properties.size_bytes;
        let original_duration = video_properties.duration.as_ref().map(|d| d.as_secs_f64());
        let original_path = media_file.path.as_ref().to_path_buf();
        let temp_path = tmp_dir.join(format!("{}.mkv", media_file_id.as_uuid()));

        // Crash recovery: detect if a previous swap completed but the DB
        // commit was lost, or if the swap was only partially done.
        let recovery_action = recovery::recover_stuck_transcode(
            &original_path,
            media_file_id,
            retention_dir,
            original_duration,
        )
        .await?;

        match recovery_action {
            RecoveryAction::ProceedNormally => {
                // Continue to normal ffmpeg encode below
            }
            RecoveryAction::RestoreAndRetry => {
                info!(
                    ?media_file_id,
                    "recovered: original restored from retention, retrying encode"
                );
                // Fall through to normal ffmpeg encode
            }
            RecoveryAction::CommitComplete {
                final_path,
                retention_path,
                output_vp,
            } => {
                let final_abs = AbsoluteFilePath::new(&final_path)?;
                return Ok(TranscodeOutcome::Completed(CompletedTranscode {
                    final_path: final_abs,
                    original_size,
                    new_size: output_vp.size_bytes,
                    bitrate: output_vp.bitrate,
                    retention_path,
                    // Recovered from a prior run; not re-measured.
                    vmaf: None,
                }));
            }
            RecoveryAction::MarkFailed { reason } => {
                return Err(TranscodeError::Recovery(reason));
            }
        }

        info!(?original_size, %crf, temp = %temp_path.display(), "starting ffmpeg encode");

        let color = media_file
            .video_properties
            .as_ref()
            .and_then(|v| v.color_metadata.as_ref());
        let mut args = encoder.build_args(crf, color);
        args.insert(0, "-nostdin".into());
        args.insert(0, "-y".into());

        let temp_guard = ScopedTemp::new(temp_path.clone());

        let output = tokio::process::Command::new("ffmpeg")
            .arg("-i")
            .arg(&original_path)
            .args(&args)
            .arg(&temp_path)
            .output()
            .await
            .map_err(|e| TranscodeError::FfmpegFailed {
                exit_code: None,
                stderr: format!("failed to spawn ffmpeg: {e}"),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            warn!(
                ?media_file_id,
                exit_code = ?output.status.code(),
                %stderr,
                "ffmpeg encode failed"
            );
            return Err(TranscodeError::FfmpegFailed {
                exit_code: output.status.code(),
                stderr,
            });
        }

        info!(?media_file_id, "ffmpeg encode finished");

        let output_vp = match validation::validate_output(&temp_path, original_duration, 1.0).await
        {
            Ok(vp) => {
                info!(?media_file_id, size = %vp.size_bytes.as_u64(), "validation passed");
                vp
            }
            Err(e) => {
                warn!(%e, ?media_file_id, "validation failed");
                return Err(TranscodeError::Validation(e));
            }
        };

        let new_size = output_vp.size_bytes;

        let min_acceptable = (original_size.as_u64() as f64 * (1.0 - min_size_reduction)) as u64;
        if new_size.as_u64() > min_acceptable {
            info!(
                ?media_file_id,
                original = %original_size.as_u64(),
                new = %new_size.as_u64(),
                threshold = %min_acceptable,
                "insufficient size reduction, skipping"
            );
            return Ok(TranscodeOutcome::Skipped {
                reason: SkipReason::InsufficientSizeReduction,
                vmaf: None,
            });
        }

        // Quality gate. The VMAF is always measured and recorded; enforcement
        // only kicks in when min_vmaf > 0 ("observe only" otherwise). A
        // measurement failure must not throw away a good encode, so we log and
        // proceed without a score.
        let vmaf = match vmaf::compute_vmaf(&original_path, &temp_path, vmaf_n_subsample).await {
            Ok(score) => {
                info!(?media_file_id, vmaf = %score, "vmaf measured");
                Some(score)
            }
            Err(e) => {
                warn!(%e, ?media_file_id, "vmaf measurement failed, proceeding without score");
                None
            }
        };

        if min_vmaf > 0.0
            && let Some(score) = vmaf
            && score < min_vmaf
        {
            info!(
                ?media_file_id,
                vmaf = %score,
                threshold = %min_vmaf,
                "vmaf below threshold, skipping"
            );
            return Ok(TranscodeOutcome::Skipped {
                reason: SkipReason::QualityTooLow,
                vmaf: Some(score),
            });
        }

        let swapper = Swapper::new(RealFileSystem);
        let result = swapper
            .atomic_swap(&original_path, &temp_path, retention_dir, media_file_id)
            .await?;

        temp_guard.disarm();

        let final_abs = AbsoluteFilePath::new(&result.final_path)?;

        Ok(TranscodeOutcome::Completed(CompletedTranscode {
            final_path: final_abs,
            original_size,
            new_size,
            bitrate: output_vp.bitrate,
            retention_path: result.retention_path,
            vmaf,
        }))
    }

    #[instrument(
        skip(store, tmp_dir, retention_dir, notifiers),
        fields(id = ?media_file_id),
        err
    )]
    #[allow(clippy::too_many_arguments)]
    async fn process_file(
        store: Arc<MediaStore>,
        encoder: Encoder,
        tmp_dir: &Path,
        retention_dir: &Path,
        min_size_reduction: f64,
        min_vmaf: f64,
        vmaf_n_subsample: u32,
        notifiers: MediaNotifiers,
        media_file_id: MediaFileId,
    ) -> Result<()> {
        let media_file = store.find_media_file_by_id(&media_file_id).await?;

        match Self::transcode_file(
            encoder,
            tmp_dir,
            retention_dir,
            min_size_reduction,
            min_vmaf,
            vmaf_n_subsample,
            &media_file,
        )
        .await
        {
            Ok(TranscodeOutcome::Completed(c)) => {
                // Record the measured quality before committing, so it's
                // queryable for accepted encodes too (calibration).
                if let Some(score) = c.vmaf
                    && let Err(e) = store.record_vmaf(&media_file_id, score).await
                {
                    warn!(%e, ?media_file_id, "failed to record vmaf");
                }
                store
                    .complete_transcode(
                        &media_file_id,
                        &c.final_path,
                        c.new_size,
                        c.bitrate.as_ref(),
                        c.original_size,
                        c.retention_path.to_str().unwrap_or(""),
                    )
                    .await?;
                info!(?media_file_id, "transcode completed successfully");

                // Best-effort: tell the media manager to pick up the new file.
                // A failure here must not fail the (already committed) transcode.
                // A media file is XOR movie/episode, so at most one of these
                // applies — Radarr for movies, Sonarr for episodes.
                if media_file.movie_id.is_some() {
                    Self::notify_movie_manager(&store, notifiers.movie.as_deref(), &media_file_id)
                        .await;
                } else if media_file.episode_id.is_some() {
                    Self::notify_series_manager(&store, notifiers.series.as_deref(), &media_file_id)
                        .await;
                }
            }
            Ok(TranscodeOutcome::Skipped { reason, vmaf }) => {
                // Record the score even on a quality reject, so the rejected
                // population is visible when calibrating the threshold.
                if let Some(score) = vmaf
                    && let Err(e) = store.record_vmaf(&media_file_id, score).await
                {
                    warn!(%e, ?media_file_id, "failed to record vmaf");
                }
                store
                    .apply_event(
                        &media_file_id,
                        WorkflowStateTag::Transcoding,
                        &MediaEvent::Skipped {
                            reason,
                            bpp: None,
                            compression_potential: None,
                        },
                    )
                    .await?;
            }
            Ok(TranscodeOutcome::AlreadyRecovered) => {
                info!(?media_file_id, "transcode already recovered");
            }
            Err(e) if e.is_terminal() => {
                error!(%e, ?media_file_id, "terminal transcode error");
                store
                    .apply_event(
                        &media_file_id,
                        WorkflowStateTag::Transcoding,
                        &MediaEvent::TranscodeFailed {
                            error: e.to_string(),
                        },
                    )
                    .await?;
            }
            Err(e) => {
                error!(%e, ?media_file_id, "transient transcode error, left in Transcoding for retry");
            }
        }

        Ok(())
    }

    /// Ask Radarr to rescan the movie this file belongs to. Best-effort and
    /// never fails the caller: a movie without a TMDB id and a
    /// missing/unconfigured notifier are silently skipped, and a failed refresh
    /// is only logged.
    async fn notify_movie_manager(
        store: &MediaStore,
        notifier: Option<&dyn MovieNotifier>,
        media_file_id: &MediaFileId,
    ) {
        let Some(notifier) = notifier else { return };

        let tmdb_id = match store.tmdb_id_for_movie_file(media_file_id).await {
            Ok(Some(id)) => id,
            Ok(None) => return,
            Err(e) => {
                warn!(%e, ?media_file_id, "failed to look up tmdb id for Radarr refresh");
                return;
            }
        };

        if let Err(e) = notifier.refresh_movie(tmdb_id).await {
            warn!(%e, tmdb_id, ?media_file_id, "failed to refresh Radarr after transcode");
        }
    }

    /// Ask Sonarr to rescan the series this episode file belongs to.
    /// Best-effort with the same semantics as [`notify_movie_manager`].
    async fn notify_series_manager(
        store: &MediaStore,
        notifier: Option<&dyn SeriesNotifier>,
        media_file_id: &MediaFileId,
    ) {
        let Some(notifier) = notifier else { return };

        let tvdb_id = match store.tvdb_id_for_episode_file(media_file_id).await {
            Ok(Some(id)) => id,
            Ok(None) => return,
            Err(e) => {
                warn!(%e, ?media_file_id, "failed to look up tvdb id for Sonarr refresh");
                return;
            }
        };

        if let Err(e) = notifier.refresh_series(tvdb_id).await {
            warn!(%e, tvdb_id, ?media_file_id, "failed to refresh Sonarr after transcode");
        }
    }
}

/// Push `id` onto `pending` unless it is already queued or currently being
/// transcoded. Returns whether it was newly enqueued. This single gate keeps
/// the periodic stale re-queue from scheduling a file twice or restarting an
/// in-progress encode.
fn enqueue_unique(
    pending: &mut VecDeque<MediaFileId>,
    inflight: &HashSet<MediaFileId>,
    id: MediaFileId,
) -> bool {
    if inflight.contains(&id) || pending.contains(&id) {
        return false;
    }
    pending.push_back(id);
    true
}

/// Guard that removes the temporary file when dropped unless explicitly
/// disarmed. Ensures cleanup on early returns without scattering
/// `remove_file` calls throughout the transcode flow.
struct ScopedTemp(PathBuf);

impl ScopedTemp {
    fn new(path: PathBuf) -> Self {
        Self(path)
    }

    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for ScopedTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    //! End-to-end tests for the core transcode logic (`transcode_file`), which
    //! is store-free and therefore needs no database — only a real `ffmpeg`.
    //! Encoder is forced to `Libx265` (software, always available, no GPU) so
    //! the tests behave identically on any host. Each test runs in an isolated
    //! temp directory and is skipped gracefully when `ffmpeg` is absent.

    use super::*;
    use crate::models::media_file::SizeBytes;
    use crate::models::video::{DurationSecs, Resolution, VideoProperties};
    use uuid::Uuid;

    // ---- enqueue_unique dedup (pure, no DB/ffmpeg) -------------------------

    #[test]
    fn enqueue_unique_adds_when_absent() {
        let mut pending = VecDeque::new();
        let inflight = HashSet::new();
        let id = MediaFileId::new();

        assert!(enqueue_unique(&mut pending, &inflight, id));
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn enqueue_unique_skips_when_already_pending() {
        let id = MediaFileId::new();
        let mut pending = VecDeque::from([id]);
        let inflight = HashSet::new();

        assert!(!enqueue_unique(&mut pending, &inflight, id));
        assert_eq!(pending.len(), 1, "must not duplicate a queued file");
    }

    #[test]
    fn enqueue_unique_skips_when_inflight() {
        let id = MediaFileId::new();
        let mut pending = VecDeque::new();
        let inflight = HashSet::from([id]);

        assert!(!enqueue_unique(&mut pending, &inflight, id));
        assert!(pending.is_empty(), "must not re-queue a running encode");
    }

    fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    struct Workspace {
        base: PathBuf,
        lib: PathBuf,
        tmp: PathBuf,
        retention: PathBuf,
    }

    impl Workspace {
        async fn new() -> Self {
            let base = std::env::temp_dir().join(format!("rk_e2e_{}", Uuid::now_v7()));
            let lib = base.join("lib");
            let tmp = base.join("tmp");
            let retention = base.join("retention");
            for d in [&lib, &tmp, &retention] {
                tokio::fs::create_dir_all(d).await.unwrap();
            }
            Self {
                base,
                lib,
                tmp,
                retention,
            }
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    /// Produces a deliberately large (near-lossless) h264 source so the HEVC
    /// re-encode is reliably smaller, keeping the `Completed` path deterministic.
    /// Output is captured (not inherited) and ffmpeg is silenced to keep test
    /// logs clean.
    async fn make_h264_source(path: &Path, secs: u32) {
        let out = tokio::process::Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostats",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size=1280x720:rate=24:duration={secs}"),
                "-c:v",
                "libx264",
                "-qp",
                "0",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(path)
            .output()
            .await
            .unwrap();
        assert!(
            out.status.success(),
            "failed to generate test source: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    async fn probe_codec(path: &Path) -> String {
        let out = tokio::process::Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=codec_name",
                "-of",
                "csv=p=0",
            ])
            .arg(path)
            .output()
            .await
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `declared_size` becomes `video_properties.size_bytes`, which drives the
    /// reduction threshold (not the real file size) — letting each test force a
    /// `Completed` or `Skipped` outcome deterministically.
    fn media_file(
        path: &Path,
        duration_secs: f64,
        declared_size: u64,
        crf: Option<u8>,
    ) -> MediaFile {
        MediaFile {
            id: MediaFileId::new(),
            episode_id: None,
            movie_id: None,
            path: AbsoluteFilePath::new(path).unwrap(),
            video_properties: Some(VideoProperties {
                video_codec: "h264".parse().unwrap(),
                resolution: Resolution::new(720, 1280).unwrap(),
                bitrate: None,
                framerate: None,
                size_bytes: SizeBytes::new(declared_size).unwrap(),
                duration: Some(DurationSecs::new(duration_secs).unwrap()),
                color_metadata: None,
                dv_profile: None,
            }),
            transcode_spec: crf.map(|c| serde_json::json!({ "crf": c })),
            workflow_state: WorkflowStateTag::Transcoding,
        }
    }

    async fn dir_is_empty(dir: &Path) -> bool {
        tokio::fs::read_dir(dir)
            .await
            .unwrap()
            .next_entry()
            .await
            .unwrap()
            .is_none()
    }

    #[tokio::test]
    async fn completes_and_swaps_to_hevc() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not available, skipping");
            return;
        }

        let ws = Workspace::new().await;
        let src = ws.lib.join("movie.mp4");
        make_h264_source(&src, 1).await;
        let real_size = tokio::fs::metadata(&src).await.unwrap().len();

        // Declared size == real size; require only a 5% reduction.
        let mf = media_file(&src, 1.0, real_size, Some(28));

        let outcome = TranscodeOrchestrator::transcode_file(
            Encoder::Libx265,
            &ws.tmp,
            &ws.retention,
            0.05,
            0.0,
            1,
            &mf,
        )
        .await
        .expect("transcode_file should succeed");

        let TranscodeOutcome::Completed(c) = outcome else {
            panic!("expected Completed, got {outcome:?}");
        };

        let final_path = c.final_path.as_ref();
        assert_eq!(final_path.extension().and_then(|e| e.to_str()), Some("mkv"));
        assert!(final_path.exists(), "final file should exist");
        assert_eq!(probe_codec(final_path).await, "hevc");
        assert!(!src.exists(), "original should have been moved out");
        assert!(c.retention_path.exists(), "backup should be in retention");
        assert!(dir_is_empty(&ws.tmp).await, "temp dir should be empty");
    }

    #[tokio::test]
    async fn skips_when_reduction_insufficient() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not available, skipping");
            return;
        }

        let ws = Workspace::new().await;
        let src = ws.lib.join("movie.mp4");
        make_h264_source(&src, 1).await;

        // Declare a tiny original size: the real HEVC output cannot beat the
        // resulting threshold, forcing the Skipped path.
        let mf = media_file(&src, 1.0, 5_000, Some(28));

        let outcome = TranscodeOrchestrator::transcode_file(
            Encoder::Libx265,
            &ws.tmp,
            &ws.retention,
            0.1,
            0.0,
            1,
            &mf,
        )
        .await
        .expect("transcode_file should succeed");

        assert!(
            matches!(
                outcome,
                TranscodeOutcome::Skipped {
                    reason: SkipReason::InsufficientSizeReduction,
                    ..
                }
            ),
            "expected Skipped, got {outcome:?}"
        );
        assert!(src.exists(), "original must be left untouched");
        assert!(dir_is_empty(&ws.retention).await, "no retention on skip");
        assert!(dir_is_empty(&ws.tmp).await, "temp dir should be cleaned");
    }

    #[tokio::test]
    async fn ffmpeg_failure_is_terminal_and_cleans_temp() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not available, skipping");
            return;
        }

        let ws = Workspace::new().await;
        let src = ws.lib.join("not-a-video.mp4");
        tokio::fs::write(&src, b"this is plain text, not a video")
            .await
            .unwrap();

        let mf = media_file(&src, 1.0, 1_000_000, Some(28));

        let err = TranscodeOrchestrator::transcode_file(
            Encoder::Libx265,
            &ws.tmp,
            &ws.retention,
            0.05,
            0.0,
            1,
            &mf,
        )
        .await
        .expect_err("ffmpeg should fail on a non-video input");

        assert!(matches!(err, TranscodeError::FfmpegFailed { .. }));
        assert!(err.is_terminal());
        assert!(dir_is_empty(&ws.tmp).await, "temp dir should be cleaned");
    }

    #[tokio::test]
    async fn missing_spec_errors_before_encoding() {
        // Returns before any ffmpeg invocation, so no availability guard needed.
        let ws = Workspace::new().await;
        let src = ws.lib.join("movie.mp4");
        tokio::fs::write(&src, b"placeholder").await.unwrap();

        let mf = media_file(&src, 1.0, 1_000_000, None);

        let err = TranscodeOrchestrator::transcode_file(
            Encoder::Libx265,
            &ws.tmp,
            &ws.retention,
            0.05,
            0.0,
            1,
            &mf,
        )
        .await
        .expect_err("missing crf should error");

        assert!(matches!(err, TranscodeError::MissingSpec));
        assert!(err.is_terminal());
    }

    // ---- Level 2: process_file end-to-end with the store -------------------
    // These exercise the full dispatcher → store effects (state transition,
    // retention row, event). They need a database and skip gracefully when
    // DATABASE_URL is unset. All DB access is scoped by media_file_id so the
    // tests are safe to run in parallel.

    use crate::store::MediaStore;
    use serial_test::serial;
    use sqlx::PgPool;

    async fn connect_db() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        PgPool::connect(&url).await.ok()
    }

    async fn insert_movie(pool: &PgPool) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO movies (id, title) VALUES ($1, $2)",
            id,
            "rankoder e2e",
        )
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn insert_transcoding_file(
        pool: &PgPool,
        movie_id: Uuid,
        path: &Path,
        declared_size: i64,
        crf: Option<i32>,
    ) -> MediaFileId {
        let id = MediaFileId::new();
        let spec = crf.map(|c| serde_json::json!({ "crf": c }));
        sqlx::query!(
            r#"
            INSERT INTO media_files
                (id, movie_id, file_path, size_bytes, video_codec, height, width,
                 duration_seconds, transcode_spec, workflow_state)
            VALUES ($1, $2, $3, $4, 'h264', 720, 1280, 1.0, $5, 'transcoding')
            "#,
            id.as_uuid(),
            movie_id,
            path.to_str().unwrap(),
            declared_size,
            spec,
        )
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn state_of(pool: &PgPool, id: MediaFileId) -> (WorkflowStateTag, String) {
        let row = sqlx::query!(
            r#"SELECT workflow_state as "ws: WorkflowStateTag", file_path
               FROM media_files WHERE id = $1"#,
            id.as_uuid(),
        )
        .fetch_one(pool)
        .await
        .unwrap();
        (row.ws, row.file_path)
    }

    async fn retention_count(pool: &PgPool, id: MediaFileId) -> i64 {
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "c!" FROM retention_files WHERE media_file_id = $1"#,
            id.as_uuid(),
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn event_count(pool: &PgPool, id: MediaFileId, event_type: &str) -> i64 {
        sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "c!" FROM events
               WHERE media_file_id = $1 AND event->>'type' = $2"#,
            id.as_uuid(),
            event_type,
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn cleanup(pool: &PgPool, movie_id: Uuid) {
        // Cascades to media_files -> events + retention_files.
        sqlx::query!("DELETE FROM movies WHERE id = $1", movie_id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn process_file_completed_marks_done_with_retention() {
        let Some(pool) = connect_db().await else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        if !ffmpeg_available() {
            eprintln!("ffmpeg not available, skipping");
            return;
        }

        let ws = Workspace::new().await;
        let src = ws.lib.join("movie.mp4");
        make_h264_source(&src, 1).await;
        let real_size = tokio::fs::metadata(&src).await.unwrap().len() as i64;

        let movie_id = insert_movie(&pool).await;
        let id = insert_transcoding_file(&pool, movie_id, &src, real_size, Some(28)).await;

        let store = Arc::new(MediaStore::new(pool.clone()));
        TranscodeOrchestrator::process_file(
            store,
            Encoder::Libx265,
            &ws.tmp,
            &ws.retention,
            0.05,
            0.0,
            1,
            MediaNotifiers::default(),
            id,
        )
        .await
        .unwrap();

        let (state, file_path) = state_of(&pool, id).await;
        assert_eq!(state, WorkflowStateTag::Done);
        assert!(file_path.ends_with(".mkv"), "file_path should point to mkv");
        assert_eq!(retention_count(&pool, id).await, 1);
        assert_eq!(event_count(&pool, id, "transcoded").await, 1);

        cleanup(&pool, movie_id).await;
    }

    #[tokio::test]
    #[serial]
    async fn process_file_skipped_marks_skipped_without_retention() {
        let Some(pool) = connect_db().await else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        if !ffmpeg_available() {
            eprintln!("ffmpeg not available, skipping");
            return;
        }

        let ws = Workspace::new().await;
        let src = ws.lib.join("movie.mp4");
        make_h264_source(&src, 1).await;

        let movie_id = insert_movie(&pool).await;
        // Tiny declared size -> reduction threshold unreachable -> Skipped.
        let id = insert_transcoding_file(&pool, movie_id, &src, 5_000, Some(28)).await;

        let store = Arc::new(MediaStore::new(pool.clone()));
        TranscodeOrchestrator::process_file(
            store,
            Encoder::Libx265,
            &ws.tmp,
            &ws.retention,
            0.1,
            0.0,
            1,
            MediaNotifiers::default(),
            id,
        )
        .await
        .unwrap();

        let (state, file_path) = state_of(&pool, id).await;
        assert_eq!(state, WorkflowStateTag::Skipped);
        assert_eq!(file_path, src.to_str().unwrap(), "original path untouched");
        assert_eq!(retention_count(&pool, id).await, 0);
        assert_eq!(event_count(&pool, id, "skipped").await, 1);

        cleanup(&pool, movie_id).await;
    }

    #[tokio::test]
    #[serial]
    async fn process_file_terminal_error_marks_failed() {
        let Some(pool) = connect_db().await else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        if !ffmpeg_available() {
            eprintln!("ffmpeg not available, skipping");
            return;
        }

        let ws = Workspace::new().await;
        let src = ws.lib.join("not-a-video.mp4");
        tokio::fs::write(&src, b"plain text, not a video")
            .await
            .unwrap();

        let movie_id = insert_movie(&pool).await;
        let id = insert_transcoding_file(&pool, movie_id, &src, 1_000_000, Some(28)).await;

        let store = Arc::new(MediaStore::new(pool.clone()));
        TranscodeOrchestrator::process_file(
            store,
            Encoder::Libx265,
            &ws.tmp,
            &ws.retention,
            0.05,
            0.0,
            1,
            MediaNotifiers::default(),
            id,
        )
        .await
        .unwrap();

        let (state, _) = state_of(&pool, id).await;
        assert_eq!(state, WorkflowStateTag::Failed);
        assert_eq!(event_count(&pool, id, "transcode_failed").await, 1);
        assert_eq!(retention_count(&pool, id).await, 0);

        cleanup(&pool, movie_id).await;
    }

    // ---- Media-manager notification ----------------------------------------

    /// Records the ids it is asked to refresh, so tests can assert the
    /// notifier was (or wasn't) invoked.
    #[derive(Default)]
    struct RecordingNotifier {
        calls: std::sync::Mutex<Vec<i32>>,
    }

    #[async_trait::async_trait]
    impl MovieNotifier for RecordingNotifier {
        async fn refresh_movie(
            &self,
            tmdb_id: i32,
        ) -> Result<(), crate::providers::ProviderError> {
            self.calls.lock().unwrap().push(tmdb_id);
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl SeriesNotifier for RecordingNotifier {
        async fn refresh_series(
            &self,
            tvdb_id: i32,
        ) -> Result<(), crate::providers::ProviderError> {
            self.calls.lock().unwrap().push(tvdb_id);
            Ok(())
        }
    }

    async fn insert_series_with_tvdb(pool: &PgPool, tvdb_id: i32) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO series (id, title, tvdb_id) VALUES ($1, $2, $3)",
            id,
            "rankoder notify series",
            tvdb_id,
        )
        .execute(pool)
        .await
        .unwrap();
        id
    }

    /// Inserts an episode under `series_id` plus a `transcoding` media file
    /// pointing at it, and returns the media file id.
    async fn insert_episode_file(pool: &PgPool, series_id: Uuid) -> MediaFileId {
        let episode_id = Uuid::now_v7();
        sqlx::query!(
            r#"INSERT INTO episodes (id, series_id, season_number, episode_number, title)
               VALUES ($1, $2, 1, 1, 'ep')"#,
            episode_id,
            series_id,
        )
        .execute(pool)
        .await
        .unwrap();

        let id = MediaFileId::new();
        sqlx::query!(
            r#"INSERT INTO media_files (id, episode_id, file_path, workflow_state)
               VALUES ($1, $2, $3, 'transcoding')"#,
            id.as_uuid(),
            episode_id,
            format!("/tmp/{}.mkv", id.as_uuid()),
        )
        .execute(pool)
        .await
        .unwrap();
        id
    }

    async fn cleanup_series(pool: &PgPool, series_id: Uuid) {
        // Cascades to episodes -> media_files.
        sqlx::query!("DELETE FROM series WHERE id = $1", series_id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn insert_movie_with_tmdb(pool: &PgPool, tmdb_id: i32) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO movies (id, title, tmdb_id) VALUES ($1, $2, $3)",
            id,
            "rankoder notify",
            tmdb_id,
        )
        .execute(pool)
        .await
        .unwrap();
        id
    }

    #[tokio::test]
    async fn notify_refreshes_movie_with_tmdb_id() {
        let Some(pool) = connect_db().await else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let store = MediaStore::new(pool.clone());

        let movie_id = insert_movie_with_tmdb(&pool, 603).await;
        let id =
            insert_transcoding_file(&pool, movie_id, Path::new("/tmp/notify.mkv"), 1_000, None).await;

        let notifier = RecordingNotifier {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        TranscodeOrchestrator::notify_movie_manager(&store, Some(&notifier), &id).await;

        assert_eq!(notifier.calls.lock().unwrap().as_slice(), &[603]);

        cleanup(&pool, movie_id).await;
    }

    #[tokio::test]
    async fn notify_skips_movie_without_tmdb_id() {
        let Some(pool) = connect_db().await else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let store = MediaStore::new(pool.clone());

        // insert_movie inserts without a tmdb_id.
        let movie_id = insert_movie(&pool).await;
        let id =
            insert_transcoding_file(&pool, movie_id, Path::new("/tmp/notify2.mkv"), 1_000, None)
                .await;

        let notifier = RecordingNotifier {
            calls: std::sync::Mutex::new(Vec::new()),
        };
        TranscodeOrchestrator::notify_movie_manager(&store, Some(&notifier), &id).await;

        assert!(notifier.calls.lock().unwrap().is_empty());

        cleanup(&pool, movie_id).await;
    }

    #[tokio::test]
    async fn notify_refreshes_series_with_tvdb_id() {
        let Some(pool) = connect_db().await else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let store = MediaStore::new(pool.clone());

        let series_id = insert_series_with_tvdb(&pool, 121361).await;
        let id = insert_episode_file(&pool, series_id).await;

        let notifier = RecordingNotifier::default();
        TranscodeOrchestrator::notify_series_manager(&store, Some(&notifier), &id).await;

        assert_eq!(notifier.calls.lock().unwrap().as_slice(), &[121361]);

        cleanup_series(&pool, series_id).await;
    }

    #[tokio::test]
    async fn notify_skips_series_without_tvdb_id() {
        let Some(pool) = connect_db().await else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let store = MediaStore::new(pool.clone());

        // insert_series_with_tvdb is the only series helper that sets tvdb_id;
        // create a series without one directly.
        let series_id = Uuid::now_v7();
        sqlx::query!(
            "INSERT INTO series (id, title) VALUES ($1, $2)",
            series_id,
            "rankoder notify no tvdb",
        )
        .execute(&pool)
        .await
        .unwrap();
        let id = insert_episode_file(&pool, series_id).await;

        let notifier = RecordingNotifier::default();
        TranscodeOrchestrator::notify_series_manager(&store, Some(&notifier), &id).await;

        assert!(notifier.calls.lock().unwrap().is_empty());

        cleanup_series(&pool, series_id).await;
    }
}
