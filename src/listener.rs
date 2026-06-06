use std::time::Duration;

use serde::Deserialize;
use sqlx::postgres::{PgListener, PgPool};
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Debug, Clone, Deserialize)]
pub struct EventNotification {
    pub event_id: i64,
    pub media_file_id: String,
    pub event_type: String,
}

pub struct PostgresListener {
    pool: PgPool,
    tx: mpsc::Sender<EventNotification>,
}

impl PostgresListener {
    pub fn new(pool: PgPool, tx: mpsc::Sender<EventNotification>) -> Self {
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
            let payload = notif.payload();

            let event: EventNotification = match serde_json::from_str(payload) {
                Ok(e) => e,
                Err(e) => {
                    warn!(
                        payload = %payload,
                        error = %e,
                        "failed to parse notification payload, skipping"
                    );
                    continue;
                }
            };

            if self.tx.send(event).await.is_err() {
                info!("notification channel closed, shutting down listener");
                return Ok(());
            }
        }
    }
}
