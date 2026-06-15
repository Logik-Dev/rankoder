use std::{sync::Arc, time::Duration};

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::store::MediaStore;

pub struct RetentionReaper {
    store: Arc<MediaStore>,
    retention_days: i32,
    interval: Duration,
}

impl RetentionReaper {
    pub fn new(store: Arc<MediaStore>, retention_days: i32) -> Self {
        Self {
            store,
            retention_days,
            interval: Duration::from_secs(3600),
        }
    }

    #[instrument(skip(self), err)]
    pub async fn run(self, token: CancellationToken) -> Result<()> {
        info!(
            retention_days = self.retention_days,
            interval_seconds = self.interval.as_secs(),
            "starting retention reaper"
        );

        let mut interval = tokio::time::interval(self.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.tick().await {
                        error!(%e, "reaper tick failed");
                    }
                }
                _ = token.cancelled() => {
                    info!("retention reaper cancelled");
                    break;
                }
            }
        }

        info!("retention reaper shut down");
        Ok(())
    }

    async fn tick(&self) -> Result<()> {
        let expired = self
            .store
            .fetch_expired_retention_files(self.retention_days)
            .await?;

        if expired.is_empty() {
            return Ok(());
        }

        info!(count = expired.len(), "reaping expired retention files");

        for (id, path) in expired {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    warn!(%path, "retention file already gone, cleaning DB entry");
                }
                Err(e) => {
                    error!(%e, %path, "failed to delete retention file from disk");
                }
            }

            self.store.delete_retention_file(&id).await?;
            info!(?id, %path, "expired retention file reaped");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::RetentionFileId;
    use sqlx::PgPool;
    use uuid::Uuid;

    struct TestFixture {
        pool: PgPool,
        movie_id: Uuid,
    }

    impl TestFixture {
        async fn new() -> Self {
            let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for tests");
            let pool = PgPool::connect(&db_url).await.expect("failed to connect");

            // Assertions are scoped by retention/media_file id, so no global
            // cleanup is needed here — a blanket DELETE would race with other
            // DB-backed tests running in parallel.

            let movie_id = Uuid::now_v7();
            sqlx::query!(
                r#"INSERT INTO movies (id, title) VALUES ($1, 'test movie')"#,
                movie_id,
            )
            .execute(&pool)
            .await
            .unwrap();

            Self { pool, movie_id }
        }

        async fn insert_media_file(&self, media_file_id: Uuid) {
            let path = format!("/tmp/{}.mkv", media_file_id);
            sqlx::query!(
                r#"
                INSERT INTO media_files (id, movie_id, file_path, workflow_state)
                VALUES ($1, $2, $3, 'done')
                "#,
                media_file_id,
                self.movie_id,
                path,
            )
            .execute(&self.pool)
            .await
            .unwrap();
        }
    }

    impl Drop for TestFixture {
        fn drop(&mut self) {
            // Nothing to do — the test DB is ephemeral
        }
    }

    #[tokio::test]
    async fn reaper_cleans_expired_files() {
        let fix = TestFixture::new().await;
        let store = Arc::new(MediaStore::new(fix.pool.clone()));

        let media_file_id = Uuid::now_v7();
        fix.insert_media_file(media_file_id).await;

        let retention_id = RetentionFileId::new();
        sqlx::query!(
            r#"
            INSERT INTO retention_files (id, media_file_id, retained_path, original_size_bytes, moved_at)
            VALUES ($1, $2, $3, $4, NOW() - make_interval(days => 10))
            "#,
            retention_id.as_uuid(),
            media_file_id,
            "/tmp/test_expired.mkv",
            1000i64,
        )
        .execute(&fix.pool)
        .await
        .unwrap();

        tokio::fs::write("/tmp/test_expired.mkv", b"dummy")
            .await
            .unwrap();

        let reaper = RetentionReaper::new(store, 7);
        reaper.tick().await.unwrap();

        let count = sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "count!" FROM retention_files WHERE id = $1"#,
            retention_id.as_uuid()
        )
        .fetch_one(&fix.pool)
        .await
        .unwrap();
        assert_eq!(count, 0, "expired retention file should be deleted");

        let _ = tokio::fs::remove_file("/tmp/test_expired.mkv").await;
    }

    #[tokio::test]
    async fn reaper_ignores_non_expired_files() {
        let fix = TestFixture::new().await;
        let store = Arc::new(MediaStore::new(fix.pool.clone()));

        let media_file_id = Uuid::now_v7();
        fix.insert_media_file(media_file_id).await;

        let retention_id = RetentionFileId::new();
        sqlx::query!(
            r#"
            INSERT INTO retention_files (id, media_file_id, retained_path, original_size_bytes, moved_at)
            VALUES ($1, $2, $3, $4, NOW() - make_interval(days => 1))
            "#,
            retention_id.as_uuid(),
            media_file_id,
            "/tmp/test_not_expired.mkv",
            1000i64,
        )
        .execute(&fix.pool)
        .await
        .unwrap();

        let reaper = RetentionReaper::new(store, 7);
        reaper.tick().await.unwrap();

        let count = sqlx::query_scalar!(
            r#"SELECT COUNT(*) as "count!" FROM retention_files WHERE id = $1"#,
            retention_id.as_uuid()
        )
        .fetch_one(&fix.pool)
        .await
        .unwrap();
        assert_eq!(count, 1, "non-expired retention file should remain");
    }
}
