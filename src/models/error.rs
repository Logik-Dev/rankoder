#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("missing uuid")]
    MissingUuid,
    #[error("given string is not a valid tmdb_id {0}")]
    InvalidTmdbId(String),
    #[error("given number is not a valid rating {0}")]
    InvalidRating(f32),
    #[error("path must be absolute: {0}")]
    InvalidPath(String),
    #[error("invalid season number: {0}")]
    InvalidSeasonNumber(i32),
    #[error("invalid episode number: {0}")]
    InvalidEpisodeNumber(i32),
    #[error("resolution is not valid")]
    InvalidResolution,
    #[error("bitrate is not valid")]
    InvalidBitrate,
    #[error("framerate is not valid")]
    InvalidFramerate,
    #[error("unknown media file status: {0}")]
    UnknownStatus(String),
    #[error("invalid size")]
    InvalidSizeBytes,
    #[error("invalid duration")]
    InvalidDuration,
}
