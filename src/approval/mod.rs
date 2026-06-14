use std::sync::Arc;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::{
    models::{batch::BatchKey, event::MediaEvent, workflow::WorkflowStateTag},
    notification::{ApprovalNotifier, ApprovalRequest, ApprovalResponse},
    store::{BatchApprovalInfo, MediaStore},
};

pub struct ApprovalOrchestrator {
    store: Arc<MediaStore>,
    notifier: Arc<dyn ApprovalNotifier>,
    wake: tokio::sync::Notify,
}

impl ApprovalOrchestrator {
    pub fn new(store: Arc<MediaStore>, notifier: Arc<dyn ApprovalNotifier>) -> Self {
        Self {
            store,
            notifier,
            wake: tokio::sync::Notify::new(),
        }
    }

    pub fn wake_feeder(&self) {
        self.wake.notify_one();
    }

    async fn publish_request(&self, request: &ApprovalRequest) -> Result<()> {
        self.notifier
            .request_approval(request)
            .await
            .map_err(Into::into)
    }

    fn build_request(key: &BatchKey, info: &BatchApprovalInfo) -> ApprovalRequest {
        ApprovalRequest {
            batch_id: key.encode(),
            title: info.title.clone(),
            file_count: info.file_count as u32,
            total_size_gb: info.total_size_gb,
            total_space_saved_gb: info.total_space_saved_gb,
            tmdb_rating: info.tmdb_rating,
        }
    }

    async fn top_up(&self, capacity: usize) {
        let pending = match self.store.count_pending_batches().await {
            Ok(n) => n,
            Err(e) => {
                error!("failed to count pending batches: {e}");
                return;
            }
        };

        let slots = capacity as i64 - pending;
        if slots <= 0 {
            return;
        }

        let keys = match self.store.fetch_ready_batch_keys(slots).await {
            Ok(v) => v,
            Err(e) => {
                error!("failed to fetch ready batch keys: {e}");
                return;
            }
        };

        for key in keys {
            let ids = match self
                .store
                .transition_batch(
                    &key,
                    WorkflowStateTag::Analyzed,
                    WorkflowStateTag::PendingApproval,
                    &MediaEvent::PendingApproval,
                )
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    error!(?key, %e, "failed to transition batch to pending approval");
                    continue;
                }
            };

            if ids.is_empty() {
                continue;
            }

            let info = match self.store.fetch_batch_request_info(&key).await {
                Ok(i) => i,
                Err(e) => {
                    error!(?key, %e, "failed to fetch batch request info");
                    continue;
                }
            };

            let request = Self::build_request(&key, &info);
            if let Err(e) = self.publish_request(&request).await {
                error!(?key, %e, "failed to publish batch approval request");
            }
        }
    }

    #[instrument(skip(self), fields(batch_id = %response.batch_id, approved = response.approved), err)]
    async fn handle_response(&self, response: ApprovalResponse) -> Result<()> {
        let key = BatchKey::decode(&response.batch_id)
            .map_err(|e| anyhow::anyhow!("invalid batch_id in response: {e}"))?;

        let (to_state, event) = if response.approved {
            (WorkflowStateTag::Transcoding, MediaEvent::ApprovalGranted)
        } else {
            (WorkflowStateTag::Skipped, MediaEvent::ApprovalRejected)
        };

        let ids = self
            .store
            .transition_batch(&key, WorkflowStateTag::PendingApproval, to_state, &event)
            .await?;

        if ids.is_empty() {
            warn!(
                ?key,
                "batch transition returned no ids (already processed?)"
            );
        }

        self.wake.notify_one();
        Ok(())
    }

    pub async fn run_response_listener(self: Arc<Self>, token: CancellationToken) -> Result<()> {
        let (tx, mut rx) = mpsc::channel::<ApprovalResponse>(32);
        let notifier = Arc::clone(&self.notifier);

        tokio::spawn(async move {
            if let Err(e) = notifier.listen_responses(tx).await {
                error!("MQTT response listener stopped: {e}");
            }
        });

        loop {
            let response = {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        info!("approval listener cancelled, shutting down");
                        return Ok(());
                    }
                    response = rx.recv() => {
                        match response {
                            Some(r) => r,
                            None => return Ok(()),
                        }
                    }
                }
            };

            if let Err(e) = self.handle_response(response).await {
                error!("failed to handle approval response: {e}");
            }
        }
    }

    pub async fn run_stale_checker(
        self: Arc<Self>,
        token: CancellationToken,
        threshold_minutes: u64,
    ) -> Result<()> {
        let mut interval =
            tokio::time::interval(std::time::Duration::from_secs(threshold_minutes * 60));
        loop {
            tokio::select! {
                biased;
                _ = token.cancelled() => return Ok(()),
                _ = interval.tick() => {
                    let keys = match self
                        .store
                        .fetch_stale_pending_batches(threshold_minutes as i32)
                        .await
                    {
                        Ok(v) => v,
                        Err(e) => {
                            error!("failed to fetch stale pending batches: {e}");
                            continue;
                        }
                    };

                    for key in keys {
                        let info = match self.store.fetch_batch_request_info(&key).await {
                            Ok(i) => i,
                            Err(e) => {
                                error!(?key, %e, "failed to fetch batch request info for stale check");
                                continue;
                            }
                        };

                        let request = Self::build_request(&key, &info);
                        if let Err(e) = self.publish_request(&request).await {
                            error!(?key, %e, "failed to re-publish stale batch request");
                        }
                    }
                }
            }
        }
    }

    pub async fn run_approval_feeder(
        self: Arc<Self>,
        token: CancellationToken,
        capacity: usize,
    ) -> Result<()> {
        loop {
            self.top_up(capacity).await;

            tokio::select! {
                biased;
                _ = token.cancelled() => return Ok(()),
                _ = self.wake.notified() => {}
            }
        }
    }
}
