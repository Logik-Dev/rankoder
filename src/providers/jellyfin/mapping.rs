use super::dto::JellyfinItem;
use crate::models::{
    common::{AbsoluteFilePath, EpisodeNumber, Rating, SeasonNumber, TmdbId},
    episode::{Episode, EpisodeId},
    movie::{Movie, MovieId},
    provider_ids::ProviderIds,
    series::{Series, SeriesId},
};
use tracing::warn;

impl From<JellyfinItem> for Series {
    fn from(item: JellyfinItem) -> Self {
        let rating = item.community_rating.and_then(|r| {
            Rating::new(r)
                .inspect_err(|error| warn!(%error, "invalid rating value"))
                .ok()
        });
        let tmdb_id = item.provider_ids.get("Tmdb").and_then(|s| {
            s.parse::<TmdbId>()
                .inspect_err(|error| warn!(%error, "failed to parse tmdb_id"))
                .ok()
        });
        let path = item.path.and_then(|p| {
            AbsoluteFilePath::new(&p)
                .inspect_err(|error| warn!(%error, "invalid path"))
                .ok()
        });

        Self {
            id: SeriesId::new(),
            title: item.name,
            rating,
            tmdb_id,
            path,
            provider_ids: ProviderIds {
                jellyfin: Some(item.id),
            },
        }
    }
}

impl From<JellyfinItem> for Movie {
    fn from(item: JellyfinItem) -> Self {
        let rating = item.community_rating.and_then(|r| {
            Rating::new(r)
                .inspect_err(|error| warn!(%error, "invalid rating value"))
                .ok()
        });
        let tmdb_id = item.provider_ids.get("Tmdb").and_then(|s| {
            s.parse::<TmdbId>()
                .inspect_err(|error| warn!(%error, "failed to parse tmdb_id"))
                .ok()
        });
        let path = item.path.and_then(|p| {
            AbsoluteFilePath::new(&p)
                .inspect_err(|error| warn!(%error, "invalid path"))
                .ok()
        });

        Self {
            id: MovieId::new(),
            title: item.name,
            rating,
            tmdb_id,
            path,
            provider_ids: ProviderIds {
                jellyfin: Some(item.id),
            },
        }
    }
}

pub(crate) fn map_to_episode(item: JellyfinItem, series_id: SeriesId) -> Episode {
    let rating = item.community_rating.and_then(|r| {
        Rating::new(r)
            .inspect_err(|error| warn!(%error, "invalid rating value"))
            .ok()
    });
    let tmdb_id = item.provider_ids.get("Tmdb").and_then(|s| {
        s.parse::<TmdbId>()
            .inspect_err(|error| warn!(%error, "failed to parse tmdb_id"))
            .ok()
    });
    let path = item.path.and_then(|p| {
        AbsoluteFilePath::new(&p)
            .inspect_err(|error| warn!(%error, "invalid path"))
            .ok()
    });
    let season_number = item.parent_index_number.and_then(|n| {
        SeasonNumber::new(n)
            .inspect_err(|error| warn!(%error, "invalid season number"))
            .ok()
    });
    let episode_number = item.index_number.and_then(|n| {
        EpisodeNumber::new(n)
            .inspect_err(|error| warn!(%error, "invalid episode number"))
            .ok()
    });

    Episode {
        id: EpisodeId::new(),
        series_id,
        season_number,
        episode_number,
        title: item.name,
        rating,
        tmdb_id,
        path,
        provider_ids: ProviderIds {
            jellyfin: Some(item.id),
        },
    }
}
