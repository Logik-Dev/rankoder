mod error;
mod jellyfin;
mod radarr;
mod sonarr;

use async_trait::async_trait;

use crate::models::{
    drafts::{EpisodeDraft, MovieDraft, SeriesDraft},
    series::SeriesId,
};

pub use error::ProviderError;
pub use jellyfin::JellyfinProvider;
pub use radarr::RadarrClient;
pub use sonarr::SonarrClient;

/// Asks Radarr to refresh a movie after its file changed (transcoded to a new
/// path/codec/size). A trait so the transcode orchestrator can be tested
/// without a live Radarr.
#[async_trait]
pub trait MovieNotifier: Send + Sync {
    /// Rescan the movie identified by its TMDB id so the manager picks up the
    /// freshly transcoded file instead of stale cached media info.
    async fn refresh_movie(&self, tmdb_id: i32) -> Result<(), ProviderError>;
}

/// Asks Sonarr to refresh a series after one of its episode files changed.
/// Sonarr rescans at series granularity (there is no per-episode rescan).
#[async_trait]
pub trait SeriesNotifier: Send + Sync {
    /// Rescan the series identified by its TVDB id so the manager picks up the
    /// freshly transcoded file instead of stale cached media info.
    async fn refresh_series(&self, tvdb_id: i32) -> Result<(), ProviderError>;
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
