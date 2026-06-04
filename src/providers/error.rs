use reqwest::header::InvalidHeaderValue;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("http request error {0}")]
    RequestError(#[from] reqwest::Error),
    #[error("tmdb id not found")]
    TmdbIdNotFound,
    #[error("invalid url")]
    InvalidUrl,
    #[error("invalid api key")]
    InvalidApiKey(#[from] InvalidHeaderValue),
    #[error("missing provider id: {0}")]
    MissingProviderId(&'static str),
}
