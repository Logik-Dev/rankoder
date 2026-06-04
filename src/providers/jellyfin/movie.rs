use async_trait::async_trait;
use tracing::instrument;

use crate::{
    models::movie::Movie,
    providers::{MovieProvider, error::ProviderError},
};

use super::JellyfinProvider;

#[async_trait]
impl MovieProvider for JellyfinProvider {
    #[instrument(skip(self), err)]
    async fn list_movies(&self) -> Result<Vec<Movie>, ProviderError> {
        let items = self
            .list_all_items_parallel("Movie", "ProviderIds,Path,CommunityRating", None)
            .await?;
        Ok(items.into_iter().map(Into::into).collect())
    }
}
