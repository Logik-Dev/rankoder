use crate::models::error::DomainError;

#[derive(Debug, thiserror::Error)]
pub enum FfprobeError {
    #[error("failed to spawn ffprobe: {0}")]
    SpawnFailed(#[from] std::io::Error),
    #[error("ffprobe process failed with exit code {0:?}")]
    ProcessFailed(Option<i32>),
    #[error("failed to parse ffprobe output: {0}")]
    InvalidOutput(#[from] serde_json::Error),
    #[error("no video stream found")]
    NoVideoStream,
    #[error("no resolution found")]
    MissingResolution,
    #[error("no size found")]
    MissingSizeBytes,
    #[error(transparent)]
    Domain(#[from] DomainError),
}
