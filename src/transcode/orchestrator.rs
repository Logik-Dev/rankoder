use std::{collections::VecDeque, path::Path, path::PathBuf, sync::Arc};

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
    store::MediaStore,
    transcode::{
        encoder::Encoder,
        error::TranscodeError,
        outcome::{CompletedTranscode, TranscodeOutcome},
        recovery::{self, RecoveryAction},
        swap::{RealFileSystem, Swapper},
        validation,
    },
};

pub struct TranscodeOrchestrator {
    rx: mpsc::Receiver<MediaFileId>,
    store: Arc<MediaStore>,
    encoder: Encoder,
    tmp_dir: PathBuf,
    retention_dir: PathBuf,
    min_size_reduction: f64,
}

impl TranscodeOrchestrator {
    pub fn new(
        rx: mpsc::Receiver<MediaFileId>,
        store: Arc<MediaStore>,
        encoder: Encoder,
        tmp_dir: PathBuf,
        retention_dir: PathBuf,
        min_size_reduction: f64,
    ) -> Self {
        Self {
            rx,
            store,
            encoder,
            tmp_dir,
            retention_dir,
            min_size_reduction,
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
        let mut join_set = JoinSet::new();

        let store = self.store;
        let encoder = self.encoder;
        let tmp_dir = self.tmp_dir;
        let retention_dir = self.retention_dir;
        let min_size_reduction = self.min_size_reduction;
        let mut rx = self.rx;

        let mut pending = VecDeque::new();

        loop {
            tokio::select! {
                biased;
                _ = token.cancelled() => {
                    info!("transcode cancelled, draining remaining tasks");
                    break;
                }
                Some(media_file_id) = rx.recv() => {
                    pending.push_back(media_file_id);
                }
                permit = semaphore.clone().acquire_owned(), if !pending.is_empty() => {
                    let media_file_id = pending.pop_front().unwrap();
                    let _permit = permit.expect("semaphore closed");
                    let s = Arc::clone(&store);
                    let enc = encoder;
                    let t = tmp_dir.clone();
                    let r = retention_dir.clone();
                    let msr = min_size_reduction;

                    join_set.spawn(async move {
                        if let Err(e) = Self::process_file(s, enc, &t, &r, msr, media_file_id).await
                        {
                            error!(%e, ?media_file_id, "transcode failed");
                        }
                    });
                }
                Some(res) = join_set.join_next() => {
                    if let Err(e) = res {
                        error!("transcode worker task panicked: {e}");
                    }
                }
                else => break,
            }
        }

        while let Some(res) = join_set.join_next().await {
            if let Err(e) = res {
                error!("transcode worker task panicked: {e}");
            }
        }

        info!("transcode orchestrator shut down");
        Ok(())
    }

    #[instrument(skip(encoder, tmp_dir, retention_dir, media_file), fields(id = ?media_file.id), err)]
    async fn transcode_file(
        encoder: Encoder,
        tmp_dir: &Path,
        retention_dir: &Path,
        min_size_reduction: f64,
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
            return Ok(TranscodeOutcome::Skipped(
                SkipReason::InsufficientSizeReduction,
            ));
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
        }))
    }

    #[instrument(skip(store, tmp_dir, retention_dir), fields(id = ?media_file_id), err)]
    async fn process_file(
        store: Arc<MediaStore>,
        encoder: Encoder,
        tmp_dir: &Path,
        retention_dir: &Path,
        min_size_reduction: f64,
        media_file_id: MediaFileId,
    ) -> Result<()> {
        let media_file = store.find_media_file_by_id(&media_file_id).await?;

        match Self::transcode_file(
            encoder,
            tmp_dir,
            retention_dir,
            min_size_reduction,
            &media_file,
        )
        .await
        {
            Ok(TranscodeOutcome::Completed(c)) => {
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
            }
            Ok(TranscodeOutcome::Skipped(reason)) => {
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
