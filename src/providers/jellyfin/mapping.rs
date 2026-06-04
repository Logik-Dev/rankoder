use std::collections::HashMap;

use tracing::warn;

use crate::{
    models::common::{AbsoluteFilePath, Rating, TmdbId},
    providers::error::ProviderError,
};

pub(crate) fn map_to_rating(community_rating: Option<f32>) -> Option<Rating> {
    community_rating.and_then(|r| {
        Rating::new(r)
            .inspect_err(|error| warn!(%error, "invalid rating value"))
            .ok()
    })
}

pub(crate) fn map_to_tmdb_id(provider_ids: HashMap<String, String>) -> Option<TmdbId> {
    provider_ids.get("Tmdb").and_then(|s| {
        s.parse::<TmdbId>()
            .inspect_err(|error| warn!(%error, "failed to parse tmdb_id"))
            .ok()
    })
}

pub(crate) fn map_to_absolute_file_path(
    path: Option<String>,
) -> Result<AbsoluteFilePath, ProviderError> {
    path.as_deref()
        .and_then(|p| {
            AbsoluteFilePath::new(p)
                .inspect_err(|error| warn!(%error, "invalid path"))
                .ok()
        })
        .ok_or(ProviderError::MissingPath)
}
