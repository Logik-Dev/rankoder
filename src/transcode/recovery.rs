use std::path::{Path, PathBuf};

use thiserror::Error;
use tracing::{error, info, warn};

use crate::{
    models::media_file::MediaFileId,
    transcode::{
        error::ValidationError,
        validation::{self, ValidatedOutput},
    },
};

#[derive(Debug)]
pub enum RecoveryAction {
    ProceedNormally,
    CommitComplete {
        final_path: PathBuf,
        retention_path: PathBuf,
        output_vp: ValidatedOutput,
    },
    RestoreAndRetry,
    MarkFailed {
        reason: String,
    },
}

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("store error: {0}")]
    Store(#[from] crate::store::error::StoreError),
    #[error("validation error: {0}")]
    Validation(#[from] ValidationError),
    #[error("filesystem error: {0}")]
    Filesystem(#[from] std::io::Error),
}

pub async fn recover_stuck_transcode(
    original_path: &Path,
    media_file_id: MediaFileId,
    retention_dir: &Path,
    original_duration: Option<f64>,
) -> Result<RecoveryAction, RecoveryError> {
    let filename = original_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");

    let retention_path = retention_dir.join(format!("{media_file_id:?}_{filename}"));
    let final_path = original_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            "{}.mkv",
            original_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("output")
        ));

    if !retention_path.exists() {
        return Ok(RecoveryAction::ProceedNormally);
    }

    info!(
        ?media_file_id,
        retention = %retention_path.display(),
        final_path = %final_path.display(),
        "detected retention file from previous swap, checking recovery state"
    );

    if final_path.exists() {
        match validation::validate_output(&final_path, original_duration, 1.0).await {
            Ok(output_vp) => {
                info!(?media_file_id, size = %output_vp.size_bytes.as_u64(), "final file exists and is valid hevc");
                Ok(RecoveryAction::CommitComplete {
                    final_path,
                    retention_path,
                    output_vp,
                })
            }
            Err(e) => {
                warn!(%e, ?media_file_id, "final file exists but validation failed");
                Ok(RecoveryAction::MarkFailed {
                    reason: format!("post-swap recovery validation failed: {e}"),
                })
            }
        }
    } else {
        warn!(
            ?media_file_id,
            original = %original_path.display(),
            retention = %retention_path.display(),
            "partial swap detected: final missing, restoring original from retention"
        );
        std::fs::rename(&retention_path, original_path).map_err(|e| {
            error!(%e, "failed to restore original from retention");
            RecoveryError::Filesystem(e)
        })?;
        info!(
            ?media_file_id,
            "original restored from retention, will retry encode"
        );
        Ok(RecoveryAction::RestoreAndRetry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_dir() -> (PathBuf, PathBuf, PathBuf) {
        let suffix = Uuid::now_v7().to_string();
        let base = std::env::temp_dir().join(format!("rankoder_recovery_{suffix}"));
        let retention_dir = base.join("retention");
        std::fs::create_dir_all(&retention_dir).unwrap();
        let original = base.join("movie.mkv");
        (base, retention_dir, original)
    }

    #[tokio::test]
    async fn no_retention_file_proceeds_normally() {
        let (base, retention_dir, original) = temp_dir();
        std::fs::write(&original, b"content").unwrap();

        let id = MediaFileId::new();
        let action = recover_stuck_transcode(&original, id, &retention_dir, None)
            .await
            .unwrap();
        assert!(matches!(action, RecoveryAction::ProceedNormally));

        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn partial_swap_restores_and_retries() {
        let (base, retention_dir, original) = temp_dir();
        let id = MediaFileId::new();
        let filename = original
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let retention_path = retention_dir.join(format!("{id:?}_{filename}"));

        std::fs::write(&retention_path, b"original content").unwrap();

        let action = recover_stuck_transcode(&original, id, &retention_dir, None)
            .await
            .unwrap();
        assert!(matches!(action, RecoveryAction::RestoreAndRetry));
        assert!(original.exists());

        std::fs::remove_dir_all(&base).ok();
    }

    #[tokio::test]
    async fn retention_and_final_present_invalid_final_fails() {
        let (base, retention_dir, original) = temp_dir();
        let id = MediaFileId::new();
        let filename = original
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let retention_path = retention_dir.join(format!("{id:?}_{filename}"));
        let final_path = base.join("movie.mkv");

        std::fs::write(&retention_path, b"original content").unwrap();
        // Write an invalid file as final — ffprobe will fail, giving MarkFailed
        std::fs::write(&final_path, b"not a real video").unwrap();

        let action = recover_stuck_transcode(&original, id, &retention_dir, None)
            .await
            .unwrap();
        assert!(matches!(action, RecoveryAction::MarkFailed { .. }));

        std::fs::remove_dir_all(&base).ok();
    }
}
