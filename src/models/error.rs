#[derive(Debug, thiserror::Error)]
pub enum DomainError {
    #[error("given string is not a valid tmdb_id {0}")]
    InvalidTmdbId(String),
    #[error("given number is not a valid rating {0}")]
    InvalidRating(f32),
    #[error("path must be absolute: {0}")]
    InvalidPath(String),
}
