use async_trait::async_trait;
use tracing::{debug, instrument};

use crate::{
    models::{
        common::{EpisodeNumber, SeasonNumber},
        drafts::{EpisodeDraft, MediaFileDraft, Provider, SeriesDraft},
        series::SeriesId,
    },
    providers::{
        SeriesProvider,
        error::ProviderError,
        jellyfin::mapping::{
            map_to_absolute_file_path, map_to_rating, map_to_tmdb_id, map_to_tvdb_id,
        },
    },
};

use super::{JellyfinItem, JellyfinProvider};

#[async_trait]
impl SeriesProvider for JellyfinProvider {
    type RawItem = JellyfinItem;

    #[instrument(skip(self), err)]
    async fn list_series(&self) -> Result<Vec<Self::RawItem>, ProviderError> {
        self.list_all_items_parallel("Series", "ProviderIds,Path,CommunityRating", None)
            .await
    }

    #[instrument(skip(self), err)]
    async fn list_episodes(&self) -> Result<Vec<Self::RawItem>, ProviderError> {
        self.list_all_items_parallel(
            "Episode",
            "ProviderIds,Path,CommunityRating,IndexNumber,ParentIndexNumber,SeriesId",
            None,
        )
        .await
    }

    fn map_to_series_draft(&self, item: JellyfinItem) -> Result<SeriesDraft, ProviderError> {
        let rating = map_to_rating(item.community_rating);
        let tmdb_id = map_to_tmdb_id(&item.provider_ids);
        let tvdb_id = map_to_tvdb_id(&item.provider_ids);

        Ok(SeriesDraft {
            title: item.name,
            provider: Provider::Jellyfin,
            tmdb_id,
            tvdb_id,
            rating,
            jellyfin_id: item.id,
        })
    }

    // `err(level = "debug")`: a mapping failure here is an expected, recoverable
    // per-item skip (e.g. Jellyfin returns an episode with no season number for
    // non-SxxEyy libraries like Kaamelott's "Livre" layout). The default `err`
    // logs every such case at ERROR, flooding each sync with dozens of scary
    // lines for something benign; the caller aggregates the count into one warn.
    #[instrument(skip(self), err(level = "debug"), fields(name = %item.name), level = "debug")]
    fn map_to_episode_draft(
        &self,
        item: JellyfinItem,
        series_id: &SeriesId,
    ) -> Result<EpisodeDraft, ProviderError> {
        let rating = map_to_rating(item.community_rating);
        let tmdb_id = map_to_tmdb_id(&item.provider_ids);
        let path = map_to_absolute_file_path(item.path)?;

        let season_number = item
            .parent_index_number
            .and_then(|n| {
                SeasonNumber::new(n)
                    .inspect_err(|error| debug!(%error, "invalid season number"))
                    .ok()
            })
            .ok_or(ProviderError::MissingSeasonNumber)?;

        let episode_number = item
            .index_number
            .and_then(|n| {
                EpisodeNumber::new(n)
                    .inspect_err(|error| debug!(%error, "invalid episode number"))
                    .ok()
            })
            .ok_or(ProviderError::MissingEpisodeNumber)?;

        Ok(EpisodeDraft {
            series_id: *series_id,
            provider: Provider::Jellyfin,
            season_number,
            episode_number,
            title: item.name,
            tmdb_id,
            rating,
            media_file: MediaFileDraft {
                jellyfin_id: item.id,
                path,
                size_bytes: None,
            },
        })
    }
}
