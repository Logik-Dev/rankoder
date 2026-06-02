mod error;
mod jellyfin;
use crate::{models::series::Series, providers::error::ProviderError};
use async_trait::async_trait;

pub use jellyfin::JellyfinProvider;

#[async_trait]
pub trait SeriesProvider {
    async fn list_series(&self) -> Result<Vec<Series>, ProviderError>;
}
