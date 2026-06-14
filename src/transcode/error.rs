use thiserror::Error;

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
    #[error("validation failed: {0}")]
    Validation(#[from] ValidationError),
    #[error("insufficient size reduction: original {original}, new {new}")]
    #[allow(dead_code)]
    InsufficientSizeReduction { original: u64, new: u64 },
    #[error("swap failed: {0}")]
    SwapFailed(#[from] std::io::Error),
    #[error("store error: {0}")]
    Store(#[from] crate::store::error::StoreError),
    #[error("probe error: {0}")]
    Probe(#[from] crate::probe::ProbeError),
    #[error("{0}")]
    Domain(#[from] crate::models::error::DomainError),
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
