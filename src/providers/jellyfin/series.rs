use async_trait::async_trait;
use tracing::instrument;

use crate::{
    models::{episode::Episode, series::Series},
    providers::{SeriesProvider, error::ProviderError},
};

use super::{JellyfinProvider, mapping::map_to_episode};

#[async_trait]
impl SeriesProvider for JellyfinProvider {
    #[instrument(skip(self), err)]
    async fn list_series(&self) -> Result<Vec<Series>, ProviderError> {
        let items = self
            .list_all_items_parallel("Series", "ProviderIds,Path,CommunityRating", None)
            .await?;
        Ok(items.into_iter().map(Into::into).collect())
    }

    #[instrument(skip(self), err)]
    async fn list_episodes(&self, series: &Series) -> Result<Vec<Episode>, ProviderError> {
        let jellyfin_id = series
            .provider_ids
            .jellyfin
            .as_deref()
            .ok_or(ProviderError::MissingProviderId("jellyfin"))?;

        let items = self
            .list_all_items_parallel(
                "Episode",
                "ProviderIds,Path,CommunityRating,IndexNumber,ParentIndexNumber",
                Some(jellyfin_id),
            )
            .await?;

        let series_id = series.id.clone();
        Ok(items
            .into_iter()
            .map(|item| map_to_episode(item, series_id.clone()))
            .collect())
    }
}
