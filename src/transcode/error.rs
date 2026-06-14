use thiserror::Error;

#[derive(Debug, Error)]
pub enum DetectError {
    #[error("no working HEVC encoder found")]
    NoEncoderAvailable,
    #[error("failed to spawn ffmpeg: {0}")]
    FfmpegSpawn(#[from] std::io::Error),
}
