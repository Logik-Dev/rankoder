use sqlx::{Postgres, Transaction};
use tracing::{info, instrument};

use crate::{
    models::{
        common::{Rating, TmdbId},
        drafts::MovieDraft,
        movie::MovieId,
    },
    store::error::StoreError,
};

#[instrument(skip(tx), err)]
pub(crate) async fn find_or_create_movie(
    tx: &mut Transaction<'_, Postgres>,
    draft: &MovieDraft,
) -> Result<MovieId, StoreError> {
    // search movie by tmdb_id
    if let Some(tmdb_id) = &draft.tmdb_id {
        let row = sqlx::query!(
            r#"SELECT id AS "id: MovieId" FROM movies WHERE tmdb_id = $1"#,
            tmdb_id as &TmdbId,
        )
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(r) = row {
            info!(
                movie_id = %r.id.as_uuid(),
                ?tmdb_id,
                jellyfin_id = %draft.jellyfin_id,
                title = %draft.title,
                "Movie already exists by tmdb_id, inserting new provider reference"
            );
            sqlx::query!(
                r#"INSERT INTO movie_provider_refs (movie_id, provider, external_id)
                   VALUES ($1, $2, $3)
                   ON CONFLICT DO NOTHING"#,
                r.id as MovieId,
                draft.provider.as_str(),
                draft.jellyfin_id,
            )
            .execute(&mut **tx)
            .await?;
            return Ok(r.id);
        }
    }

    // else search by jellyfin_id
    let row = sqlx::query!(
        r#"SELECT movie_id AS "movie_id: MovieId"
           FROM movie_provider_refs
           WHERE provider = $1 AND external_id = $2"#,
        draft.provider.as_str(),
        draft.jellyfin_id,
    )
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(r) = row {
        return Ok(r.movie_id);
    }

    // else insert
    let new_id = MovieId::new();
    sqlx::query!(
        r#"INSERT INTO movies (id, title, tmdb_id, rating)
           VALUES ($1, $2, $3, $4)"#,
        new_id as MovieId,
        draft.title,
        draft.tmdb_id.as_ref() as Option<&TmdbId>,
        draft.rating.as_ref() as Option<&Rating>,
    )
    .execute(&mut **tx)
    .await?;

    // link provider ref (only jellyfin for now) TODO
    sqlx::query!(
        r#"INSERT INTO movie_provider_refs (movie_id, provider, external_id)
           VALUES ($1, $2, $3)"#,
        new_id as MovieId,
        draft.provider.as_str(),
        draft.jellyfin_id
    )
    .execute(&mut **tx)
    .await?;

    Ok(new_id)
}
