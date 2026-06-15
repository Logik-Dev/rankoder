use std::{sync::Arc, time::Duration};

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::{
    models::workflow::WorkflowStateTag,
    notification::{FailureAlert, StatusNotifier, StatusSnapshot},
    store::{FailureRecord, MediaStore},
};

const REPORT_INTERVAL: Duration = Duration::from_secs(60);
const BYTES_PER_GB: f64 = 1_000_000_000.0;

/// Periodically surfaces pipeline state to the operator, fully decoupled from
/// the transcode path: it reads the `events` table and publishes two things via
/// a [`StatusNotifier`] — an immediate alert per new failure, and a retained
/// status snapshot.
pub struct StatusReporter {
    store: Arc<MediaStore>,
    notifier: Arc<dyn StatusNotifier>,
    interval: Duration,
}

impl StatusReporter {
    pub fn new(store: Arc<MediaStore>, notifier: Arc<dyn StatusNotifier>) -> Self {
        Self {
            store,
            notifier,
            interval: REPORT_INTERVAL,
        }
    }

    pub async fn run(self, token: CancellationToken) -> Result<()> {
        // Seed the high-water mark at the latest event so a restart doesn't
        // re-alert historical failures. The retained snapshot still surfaces
        // the standing `failed` count and the most recent failure.
        let mut last_event_id = self.store.fetch_max_event_id().await.unwrap_or(0);
        info!(
            interval_seconds = self.interval.as_secs(),
            last_event_id, "starting status reporter"
        );

        let mut interval = tokio::time::interval(self.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = token.cancelled() => {
                    info!("status reporter cancelled");
                    break;
                }
                _ = interval.tick() => {
                    last_event_id = self.push_new_failures(last_event_id).await;
                    if let Err(e) = self.push_status().await {
                        warn!(%e, "failed to publish status snapshot");
                    }
                }
            }
        }

        info!("status reporter shut down");
        Ok(())
    }

    /// Publish an alert for each failure newer than `after_id`; returns the new
    /// high-water mark. Best-effort: a publish failure is logged, never fatal.
    async fn push_new_failures(&self, after_id: i64) -> i64 {
        let failures = match self.store.fetch_failures_after(after_id).await {
            Ok(f) => f,
            Err(e) => {
                error!(%e, "failed to fetch new failures");
                return after_id;
            }
        };

        let mut high = after_id;
        for failure in failures {
            high = high.max(failure.event_id);
            let alert = to_alert(failure);
            if let Err(e) = self.notifier.publish_failure(&alert).await {
                warn!(%e, "failed to publish failure alert");
            }
        }
        high
    }

    async fn push_status(&self) -> Result<()> {
        let counts = self.store.fetch_state_counts().await?;
        let saved_bytes = self.store.fetch_total_space_saved_bytes().await?;
        let last_failure = self.store.fetch_last_failure().await?.map(to_alert);

        let mut snapshot = StatusSnapshot {
            space_saved_gb: saved_bytes as f64 / BYTES_PER_GB,
            last_failure,
            ..Default::default()
        };

        for (state, count) in counts {
            match state {
                WorkflowStateTag::Discovered => snapshot.discovered = count,
                WorkflowStateTag::Probed => snapshot.probed = count,
                WorkflowStateTag::Analyzed => snapshot.analyzed = count,
                WorkflowStateTag::PendingApproval => snapshot.pending_approval = count,
                WorkflowStateTag::Transcoding => snapshot.transcoding = count,
                WorkflowStateTag::Done => snapshot.done = count,
                WorkflowStateTag::Skipped => snapshot.skipped = count,
                WorkflowStateTag::Failed => snapshot.failed = count,
            }
        }

        self.notifier.publish_status(&snapshot).await?;
        Ok(())
    }
}

fn to_alert(failure: FailureRecord) -> FailureAlert {
    FailureAlert {
        media_file_id: failure.media_file_id.as_uuid().to_string(),
        kind: failure.kind,
        title: failure.title,
        reason: failure.error,
    }
}
