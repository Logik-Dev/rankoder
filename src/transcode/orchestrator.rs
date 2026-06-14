use std::{collections::VecDeque, path::Path, path::PathBuf, sync::Arc};

use anyhow::Result;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::{
    models::{
        common::AbsoluteFilePath, event::MediaEvent, media_file::MediaFileId,
        transcode::SkipReason, workflow::WorkflowStateTag,
    },
    store::MediaStore,
    transcode::{
        encoder::Encoder,
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

        let video_properties = media_file
            .video_properties
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no video properties for file {media_file_id:?}"))?;

        let original_size = video_properties.size_bytes;
        let original_duration = video_properties.duration.as_ref().map(|d| d.as_secs_f64());

        let crf = media_file
            .transcode_spec
            .as_ref()
            .and_then(|s| s.get("crf"))
            .and_then(|c| c.as_u64())
            .map(|c| c as u8)
            .unwrap_or(23);

        let temp_path = tmp_dir.join(format!("{media_file_id:?}.mkv"));
        let original_path = media_file.path.as_ref().to_path_buf();

        info!(?original_size, %crf, temp = %temp_path.display(), "starting ffmpeg encode");

        // Encode step
        let mut args = encoder.build_args(crf);
        args.insert(0, "-nostdin".into());
        args.insert(0, "-y".into());

        let output = tokio::process::Command::new("ffmpeg")
            .arg("-i")
            .arg(&original_path)
            .args(&args)
            .arg(&temp_path)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            warn!(
                ?media_file_id,
                exit_code = ?output.status.code(),
                %stderr,
                "ffmpeg encode failed"
            );
            let _ = tokio::fs::remove_file(&temp_path).await;
            store
                .transition(
                    &media_file_id,
                    WorkflowStateTag::Transcoding,
                    WorkflowStateTag::Failed,
                    &MediaEvent::TranscodeFailed {
                        error: format!("ffmpeg exit code {:?}: {}", output.status.code(), stderr),
                    },
                )
                .await?;
            return Ok(());
        }

        info!(?media_file_id, "ffmpeg encode finished");

        // Validation step
        let output_vp = match validation::validate_output(&temp_path, original_duration, 1.0).await
        {
            Ok(vp) => {
                info!(?media_file_id, size = %vp.size_bytes.as_u64(), "validation passed");
                vp
            }
            Err(e) => {
                warn!(%e, ?media_file_id, "validation failed");
                let _ = tokio::fs::remove_file(&temp_path).await;
                store
                    .transition(
                        &media_file_id,
                        WorkflowStateTag::Transcoding,
                        WorkflowStateTag::Failed,
                        &MediaEvent::TranscodeFailed {
                            error: e.to_string(),
                        },
                    )
                    .await?;
                return Ok(());
            }
        };

        let new_size = output_vp.size_bytes;

        // Size reduction threshold check
        let min_acceptable = (original_size.as_u64() as f64 * (1.0 - min_size_reduction)) as u64;
        if new_size.as_u64() > min_acceptable {
            info!(
                ?media_file_id,
                original = %original_size.as_u64(),
                new = %new_size.as_u64(),
                threshold = %min_acceptable,
                "insufficient size reduction, skipping"
            );
            let _ = tokio::fs::remove_file(&temp_path).await;
            store
                .transition(
                    &media_file_id,
                    WorkflowStateTag::Transcoding,
                    WorkflowStateTag::Skipped,
                    &MediaEvent::Skipped {
                        reason: SkipReason::InsufficientSizeReduction,
                        bpp: None,
                        compression_potential: None,
                    },
                )
                .await?;
            return Ok(());
        }

        // Atomic swap
        let swapper = Swapper::new(RealFileSystem);
        let result = swapper
            .atomic_swap(&original_path, &temp_path, retention_dir, media_file_id)
            .await?;

        let final_abs = AbsoluteFilePath::new(&result.final_path)?;

        store
            .complete_transcode(
                &media_file_id,
                &final_abs,
                new_size,
                output_vp.bitrate.as_ref(),
                original_size,
                result.retention_path.to_str().unwrap_or(""),
            )
            .await
            .map_err(|e| {
                error!(%e, "complete_transcode failed after swap");
                anyhow::anyhow!("complete_transcode failed: {e}")
            })?;

        info!(?media_file_id, "transcode completed successfully");
        Ok(())
    }
}
