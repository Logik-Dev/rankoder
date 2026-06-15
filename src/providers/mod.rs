mod error;
mod jellyfin;
mod radarr;

use async_trait::async_trait;

use crate::models::{
    drafts::{EpisodeDraft, MovieDraft, SeriesDraft},
    series::SeriesId,
};

pub use error::ProviderError;
pub use jellyfin::JellyfinProvider;
pub use radarr::RadarrClient;

/// Asks a downstream media manager to refresh its view of a title after the
/// underlying file changed (transcoded to a new path/codec/size). Implemented
/// by `RadarrClient`; kept as a trait so the transcode orchestrator can be
/// tested without a live Radarr and so Sonarr/Jellyfin can be added later.
#[async_trait]
pub trait MediaServerNotifier: Send + Sync {
    /// Rescan the movie identified by its TMDB id so the manager picks up the
    /// freshly transcoded file instead of stale cached media info.
    async fn refresh_movie(&self, tmdb_id: i32) -> Result<(), ProviderError>;
}

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
