use std::{sync::Arc, time::Duration};

use serde::Deserialize;
use sqlx::postgres::{PgListener, PgPool};
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{models::media_file::MediaFileId, store::MediaStore};

#[derive(Debug, Clone, Deserialize)]
pub struct EventNotification {
    #[allow(dead_code)]
    pub event_id: i64,
    pub media_file_id: Uuid,
    #[allow(dead_code)]
    pub event_type: String,
}

pub struct PostgresListener {
    pool: PgPool,
    store: Arc<MediaStore>,
    tx: mpsc::Sender<MediaFileId>,
}

impl PostgresListener {
    pub fn new(pool: PgPool, store: Arc<MediaStore>, tx: mpsc::Sender<MediaFileId>) -> Self {
        Self { pool, store, tx }
    }

    pub async fn listen(self) -> anyhow::Result<()> {
        loop {
            if let Err(e) = self.run_listener().await {
                warn!(error = %e, "Postgres listener error, reconnecting in 1s");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn catch_up(&self) -> anyhow::Result<()> {
        let ids = self.store.fetch_active_media_files().await?;
        let count = ids.len();
        for id in ids {
            if self.tx.send(id).await.is_err() {
                info!("notification channel closed during catch-up");
                return Ok(());
            }
        }
        info!(count, "caught up active media files");
        Ok(())
    }

    async fn run_listener(&self) -> anyhow::Result<()> {
        let mut listener = PgListener::connect_with(&self.pool).await?;
        listener.listen("media_event").await?;
        info!("listening on media_event channel");

        self.catch_up().await?;

        loop {
            let notif = listener.recv().await?;
            let notification: EventNotification = serde_json::from_str(notif.payload())?;
            let media_file_id = MediaFileId::from(notification.media_file_id);

            if self.tx.send(media_file_id).await.is_err() {
                info!("notification channel closed, shutting down listener");
                return Ok(());
            }
        }
    }
}
