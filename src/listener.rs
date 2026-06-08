use std::time::Duration;

use serde::Deserialize;
use sqlx::postgres::{PgListener, PgPool};
use tokio::sync::mpsc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::models::{error::DomainError, media_file::MediaFileId};

#[derive(Debug, Clone, Deserialize)]
pub struct EventNotification {
    pub event_id: i64,
    pub media_file_id: String,
    pub event_type: String,
}

pub struct PostgresListener {
    pool: PgPool,
    tx: mpsc::Sender<MediaFileId>,
}

impl PostgresListener {
    pub fn new(pool: PgPool, tx: mpsc::Sender<MediaFileId>) -> Self {
        Self { pool, tx }
    }

    pub async fn listen(self) -> anyhow::Result<()> {
        loop {
            if let Err(e) = self.run_listener().await {
                warn!(error = %e, "Postgres listener error, reconnecting in 1s");
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn run_listener(&self) -> anyhow::Result<()> {
        let mut listener = PgListener::connect_with(&self.pool).await?;
        listener.listen("media_event").await?;
        info!("listening on media_event channel");

        loop {
            let notif = listener.recv().await?;
            let payload: serde_json::Value = serde_json::from_str(notif.payload())?;

            let media_file_id = match payload["media_file_id"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or(DomainError::MissingUuid)
                .map(MediaFileId::from)
            {
                Ok(id) => id,
                Err(e) => {
                    warn!(
                        payload = %payload,
                        error = %e,
                        "failed to parse notification payload, skipping"
                    );
                    continue;
                }
            };

            if self.tx.send(media_file_id).await.is_err() {
                info!("notification channel closed, shutting down listener");
                return Ok(());
            }
        }
    }
}
