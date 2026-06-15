use sqlx::{Postgres, Transaction};
use tracing::{debug, instrument};

use crate::{
    models::{
        common::{Rating, TmdbId, TvdbId},
        drafts::SeriesDraft,
        series::SeriesId,
    },
    store::error::StoreError,
};

#[instrument(skip(tx), err)]
pub(crate) async fn find_or_create_series(
    tx: &mut Transaction<'_, Postgres>,
    draft: &SeriesDraft,
) -> Result<SeriesId, StoreError> {
    if let Some(tmdb_id) = &draft.tmdb_id {
        let row = sqlx::query!(
            r#"SELECT id AS "id: SeriesId" FROM series WHERE tmdb_id = $1"#,
            tmdb_id as &TmdbId,
        )
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(r) = row {
            debug!(
                series_id = %r.id.as_uuid(),
                ?tmdb_id,
                jellyfin_id = %draft.jellyfin_id,
                title = %draft.title,
                "Series already exists by tmdb_id, inserting new provider reference"
            );
            sqlx::query!(
                r#"INSERT INTO series_provider_refs (series_id, provider, external_id)
                   VALUES ($1, $2, $3)
                   ON CONFLICT DO NOTHING"#,
                r.id as SeriesId,
                draft.provider.as_str(),
                draft.jellyfin_id,
            )
            .execute(&mut **tx)
            .await?;
            return Ok(r.id);
        }
    }

    let row = sqlx::query!(
        r#"SELECT series_id AS "series_id: SeriesId"
           FROM series_provider_refs
           WHERE provider = $1 AND external_id = $2"#,
        draft.provider.as_str(),
        draft.jellyfin_id,
    )
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(r) = row {
        return Ok(r.series_id);
    }

    let new_id = SeriesId::new();
    sqlx::query!(
        r#"INSERT INTO series (id, title, tmdb_id, tvdb_id, rating)
           VALUES ($1, $2, $3, $4, $5)"#,
        new_id as SeriesId,
        draft.title,
        draft.tmdb_id.as_ref() as Option<&TmdbId>,
        draft.tvdb_id.as_ref() as Option<&TvdbId>,
        draft.rating.as_ref() as Option<&Rating>,
    )
    .execute(&mut **tx)
    .await?;

    sqlx::query!(
        r#"INSERT INTO series_provider_refs (series_id, provider, external_id)
           VALUES ($1, $2, $3)"#,
        new_id as SeriesId,
        draft.provider.as_str(),
        draft.jellyfin_id
    )
    .execute(&mut **tx)
    .await?;

    Ok(new_id)
}
