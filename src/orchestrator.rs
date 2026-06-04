use std::collections::HashMap;

use anyhow::Result;
use tracing::{info, instrument, warn};

use crate::{
    models::{
        drafts::{self, MovieDraft, SeriesDraft},
        series::SeriesId,
    },
    providers::{MovieProvider, ParentId, SeriesProvider},
    store::MediaStore,
};

pub struct SyncOrchestrator<S, M> {
    series_provider: S,
    movie_provider: M,
    store: MediaStore,
}

impl<S, M> SyncOrchestrator<S, M>
where
    S: SeriesProvider,
    M: MovieProvider,
{
    pub fn new(series_provider: S, movie_provider: M, store: MediaStore) -> Self {
        Self {
            series_provider,
            movie_provider,
            store,
        }
    }

    #[instrument(skip(self), err)]
    pub async fn sync(&self) -> Result<()> {
        info!("start syncing libraries");

        let (series_raw, episodes_raw, movies_raw) = tokio::try_join!(
            self.series_provider.list_series(),
            self.series_provider.list_episodes(),
            self.movie_provider.list_movies(),
        )?;

        let series_map = self.sync_series(series_raw).await?;
        let (episode_count, movie_count) = tokio::try_join!(
            self.sync_episodes(episodes_raw, &series_map),
            self.sync_movies(movies_raw),
        )?;

        info!(
            series = series_map.len(),
            episodes = episode_count,
            movies = movie_count,
            "synced"
        );

        Ok(())
    }

    #[instrument(skip(self, raw), fields(raw_items = %raw.len()))]
    async fn sync_series(&self, raw: Vec<S::RawItem>) -> Result<HashMap<String, SeriesId>> {
        info!("start syncing series library");

        let drafts: Vec<SeriesDraft> = raw
            .into_iter()
            .map(|item| self.series_provider.map_to_series_draft(item))
            .collect::<Result<Vec<_>, _>>()?;

        let map = self.store.insert_series_batched(&drafts, 500).await?;
        Ok(map)
    }

    #[instrument(skip(self, raw, series_map), fields(raw_items = %raw.len()))]
    async fn sync_episodes(
        &self,
        raw: Vec<S::RawItem>,
        series_map: &HashMap<String, SeriesId>,
    ) -> Result<usize> {
        info!("start syncing episodes library");

        let mut drafts = Vec::new();
        for item in raw {
            let Some(jellyfin_series_id) = item.parent_id() else {
                warn!("skipping episode without series_id");
                continue;
            };
            let Some(series_id) = series_map.get(jellyfin_series_id) else {
                warn!(%jellyfin_series_id, "skipping episode referencing unknown series");
                continue;
            };
            let Ok(draft) = self.series_provider.map_to_episode_draft(item, series_id) else {
                warn!("skipping invalid episode");
                continue;
            };
            drafts.push(draft);
        }
        let count = drafts.len();
        self.store.insert_episodes_batched(&drafts, 500).await?;
        Ok(count)
    }

    #[instrument(skip(self, raw), fields(raw_items = %raw.len()))]
    async fn sync_movies(&self, raw: Vec<M::RawItem>) -> Result<usize> {
        info!("start syncing movies library");

        let drafts: Vec<MovieDraft> = raw
            .into_iter()
            .map(|item| self.movie_provider.map_to_movie_draft(item))
            .collect::<Result<Vec<_>, _>>()?;

        let count = drafts.len();
        self.store.insert_movies_batched(&drafts, 500).await?;
        Ok(count)
    }
}
