mod error;
mod jellyfin;
use crate::{
    models::{episode::Episode, series::Series},
    providers::error::ProviderError,
};
use async_trait::async_trait;

pub use jellyfin::JellyfinProvider;

#[async_trait]
pub trait SeriesProvider {
    async fn list_series(&self) -> Result<Vec<Series>, ProviderError>;
    async fn list_episodes(&self, series: &Series) -> Result<Vec<Episode>, ProviderError>;
}
