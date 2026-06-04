use sqlx::{Postgres, Transaction};
use tracing::instrument;

use crate::{
    models::{
        common::{Rating, TmdbId},
        drafts::EpisodeDraft,
        episode::EpisodeId,
        series::SeriesId,
    },
    store::error::StoreError,
};

#[instrument(skip(tx), err)]
pub(crate) async fn find_or_create_episode(
    tx: &mut Transaction<'_, Postgres>,
    draft: &EpisodeDraft,
) -> Result<EpisodeId, StoreError> {
    let season = draft.season_number.as_i16();
    let episode = draft.episode_number.as_i16();

    let row = sqlx::query!(
        r#"SELECT id AS "id: EpisodeId"
           FROM episodes
           WHERE series_id = $1 AND season_number = $2 AND episode_number = $3"#,
        draft.series_id as SeriesId,
        season,
        episode,
    )
    .fetch_optional(&mut **tx)
    .await?;

    if let Some(r) = row {
        return Ok(r.id);
    }

    let new_id = EpisodeId::new();
    sqlx::query!(
        r#"INSERT INTO episodes (id, series_id, season_number, episode_number, title, tmdb_id, rating)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
        new_id as EpisodeId,
        draft.series_id as SeriesId,
        season,
        episode,
        draft.title,
        draft.tmdb_id.as_ref() as Option<&TmdbId>,
        draft.rating.as_ref() as Option<&Rating>,
    )
    .execute(&mut **tx)
    .await?;

    Ok(new_id)
}
