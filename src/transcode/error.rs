use thiserror::Error;

use crate::transcode::recovery::RecoveryError;

#[derive(Debug, Error)]
pub enum DetectError {
    #[error("no working HEVC encoder found")]
    NoEncoderAvailable,
    #[error("failed to spawn ffmpeg: {0}")]
    FfmpegSpawn(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum TranscodeError {
    #[error("ffmpeg failed with exit code {exit_code:?}: {stderr}")]
    #[allow(dead_code)]
    FfmpegFailed {
        exit_code: Option<i32>,
        stderr: String,
    },
    #[error("video properties missing for media file")]
    #[allow(dead_code)]
    MissingVideoProperties,
    #[error("missing transcode spec or crf")]
    #[allow(dead_code)]
    MissingSpec,
    #[error("validation failed: {0}")]
    Validation(#[from] ValidationError),
    #[error("recovery failed: {0}")]
    Recovery(String),
    #[error("swap failed: {0}")]
    SwapFailed(#[from] std::io::Error),
    #[error("store error: {0}")]
    Store(#[from] crate::store::error::StoreError),
    #[error("probe error: {0}")]
    Probe(#[from] crate::probe::ProbeError),
    #[error("{0}")]
    Domain(#[from] crate::models::error::DomainError),
}

impl TranscodeError {
    /// Terminal errors are recorded as `Failed`. Transient errors are propagated
    /// so that the file remains in `Transcoding` and can be retried.
    #[allow(dead_code)]
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::FfmpegFailed { .. }
                | Self::MissingVideoProperties
                | Self::MissingSpec
                | Self::Validation(_)
                | Self::Recovery(_)
                | Self::SwapFailed(_)
                | Self::Domain(_)
        )
    }
}

impl From<RecoveryError> for TranscodeError {
    fn from(e: RecoveryError) -> Self {
        match e {
            RecoveryError::Validation(v) => Self::Validation(v),
            RecoveryError::Filesystem(io) => Self::Recovery(format!("filesystem error: {io}")),
            RecoveryError::Store(s) => Self::Store(s),
        }
    }
}

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("wrong codec, expected hevc")]
    WrongCodec,
    #[error("duration mismatch: original {original:.1}s, new {new:.1}s")]
    DurationMismatch { original: f64, new: f64 },
    #[error("ffprobe failed: {0}")]
    FfprobeFailed(String),
}
