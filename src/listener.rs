use std::{sync::Arc, time::Duration};

use serde::Deserialize;
use sqlx::postgres::PgListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
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
    database_url: String,
    store: Arc<MediaStore>,
    tx: mpsc::Sender<MediaFileId>,
}

impl PostgresListener {
    pub fn new(
        database_url: String,
        store: Arc<MediaStore>,
        tx: mpsc::Sender<MediaFileId>,
    ) -> Self {
        Self {
            database_url,
            store,
            tx,
        }
    }

    pub async fn listen(self, token: CancellationToken) -> anyhow::Result<()> {
        loop {
            if token.is_cancelled() {
                info!("listener cancelled, shutting down");
                return Ok(());
            }
            if let Err(e) = self.run_listener(&token).await {
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

    async fn run_listener(&self, token: &CancellationToken) -> anyhow::Result<()> {
        let mut listener = PgListener::connect(&self.database_url).await?;
        listener.listen("media_event").await?;
        info!("listening on media_event channel");

        self.catch_up().await?;

        loop {
            let notif = {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        info!("listener cancelled during recv");
                        return Ok(());
                    }
                    notif = listener.recv() => {
                        notif?
                    }
                }
            };

            let notification: EventNotification = serde_json::from_str(notif.payload())?;
            let media_file_id = MediaFileId::from(notification.media_file_id);

            if self.tx.send(media_file_id).await.is_err() {
                info!("notification channel closed, shutting down listener");
                return Ok(());
            }
        }
    }
}
