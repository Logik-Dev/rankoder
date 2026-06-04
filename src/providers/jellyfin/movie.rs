use async_trait::async_trait;
use tracing::instrument;

use crate::{
    models::drafts::{MediaFileDraft, MovieDraft, Provider},
    providers::{
        MovieProvider,
        error::ProviderError,
        jellyfin::mapping::{map_to_absolute_file_path, map_to_rating, map_to_tmdb_id},
    },
};

use super::{JellyfinItem, JellyfinProvider};

#[async_trait]
impl MovieProvider for JellyfinProvider {
    type RawItem = JellyfinItem;

    #[instrument(skip(self), err)]
    async fn list_movies(&self) -> Result<Vec<Self::RawItem>, ProviderError> {
        self.list_all_items_parallel("Movie", "ProviderIds,Path,CommunityRating", None)
            .await
    }

    fn map_to_movie_draft(&self, item: JellyfinItem) -> Result<MovieDraft, ProviderError> {
        let rating = map_to_rating(item.community_rating);
        let tmdb_id = map_to_tmdb_id(item.provider_ids);
        let path = map_to_absolute_file_path(item.path)?;

        Ok(MovieDraft {
            title: item.name,
            provider: Provider::Jellyfin,
            tmdb_id,
            rating,
            jellyfin_id: item.id.clone(),
            media_file: MediaFileDraft {
                jellyfin_id: item.id,
                path,
                size_bytes: None,
            },
        })
    }
}
