use sqlx::{Postgres, Transaction};

use crate::{
    models::{
        drafts::{EpisodeDraft, MovieDraft},
        episode::EpisodeId,
        event::MediaEvent,
        media_file::MediaFileId,
        movie::MovieId,
    },
    store::error::StoreError,
};

pub(crate) async fn upsert_movie_file(
    tx: &mut Transaction<'_, Postgres>,
    movie_id: MovieId,
    draft: &MovieDraft,
) -> Result<MediaFileId, StoreError> {
    let new_id = MediaFileId::new();
    let file_path = draft.media_file.path.as_path().to_string_lossy();

    let row = sqlx::query!(
        r#"INSERT INTO media_files (id, movie_id, file_path, size_bytes, jellyfin_id)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (jellyfin_id) WHERE jellyfin_id IS NOT NULL
           DO UPDATE SET
               last_seen_at = NOW(),
               file_path = EXCLUDED.file_path,
               size_bytes = EXCLUDED.size_bytes
           RETURNING id AS "id: MediaFileId", (xmax = 0) AS "was_inserted!: bool""#,
        new_id as MediaFileId,
        movie_id as MovieId,
        file_path.as_ref(),
        draft.media_file.size_bytes,
        draft.media_file.jellyfin_id,
    )
    .fetch_one(&mut **tx)
    .await?;

    if row.was_inserted {
        let event = MediaEvent::Discovered {
            source: draft.provider.as_str().into(),
        };
        sqlx::query!(
            r#"INSERT INTO events (media_file_id, event)
               VALUES ($1, $2)"#,
            row.id as MediaFileId,
            serde_json::to_value(&event)? as _,
        )
        .execute(&mut **tx)
        .await?;
    }

    Ok(row.id)
}

pub(crate) async fn upsert_episode_file(
    tx: &mut Transaction<'_, Postgres>,
    episode_id: EpisodeId,
    draft: &EpisodeDraft,
) -> Result<MediaFileId, StoreError> {
    let new_id = MediaFileId::new();
    let file_path = draft.media_file.path.as_path().to_string_lossy();

    let row = sqlx::query!(
        r#"INSERT INTO media_files (id, episode_id, file_path, size_bytes, jellyfin_id)
           VALUES ($1, $2, $3, $4, $5)
           ON CONFLICT (jellyfin_id) WHERE jellyfin_id IS NOT NULL
           DO UPDATE SET
               last_seen_at = NOW(),
               file_path = EXCLUDED.file_path,
               size_bytes = EXCLUDED.size_bytes
           RETURNING id AS "id: MediaFileId", (xmax = 0) AS "was_inserted!: bool""#,
        new_id as MediaFileId,
        episode_id as EpisodeId,
        file_path.as_ref(),
        draft.media_file.size_bytes,
        draft.media_file.jellyfin_id,
    )
    .fetch_one(&mut **tx)
    .await?;

    if row.was_inserted {
        let event = MediaEvent::Discovered {
            source: draft.provider.as_str().into(),
        };
        sqlx::query!(
            r#"INSERT INTO events (media_file_id, event)
               VALUES ($1, $2)"#,
            row.id as MediaFileId,
            serde_json::to_value(&event)? as _,
        )
        .execute(&mut **tx)
        .await?;
    }

    Ok(row.id)
}
