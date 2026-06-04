mod error;
mod jellyfin;

use async_trait::async_trait;

use crate::{
    models::{
        drafts::{EpisodeDraft, MovieDraft, SeriesDraft},
        series::SeriesId,
    },
    providers::error::ProviderError,
};

pub use jellyfin::JellyfinProvider;

pub trait ParentId {
    fn parent_id(&self) -> Option<&str>;
}

#[async_trait]
pub trait SeriesProvider {
    type RawItem: ParentId;

    async fn list_series(&self) -> Result<Vec<Self::RawItem>, ProviderError>;
    async fn list_episodes(&self) -> Result<Vec<Self::RawItem>, ProviderError>;
    fn map_to_series_draft(&self, item: Self::RawItem) -> Result<SeriesDraft, ProviderError>;
    fn map_to_episode_draft(
        &self,
        item: Self::RawItem,
        series_id: &SeriesId,
    ) -> Result<EpisodeDraft, ProviderError>;
}

#[async_trait]
pub trait MovieProvider {
    type RawItem;

    async fn list_movies(&self) -> Result<Vec<Self::RawItem>, ProviderError>;
    fn map_to_movie_draft(&self, item: Self::RawItem) -> Result<MovieDraft, ProviderError>;
}
