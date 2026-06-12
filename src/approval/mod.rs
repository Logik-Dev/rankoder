use std::sync::Arc;

use anyhow::{Result, bail};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::{
    models::{
        event::MediaEvent,
        media_file::{MediaFile, MediaFileId},
        workflow::WorkflowStateTag,
    },
    notification::{ApprovalNotifier, ApprovalRequest, ApprovalResponse},
    store::{MediaStore, error::StoreError},
};

pub struct ApprovalOrchestrator {
    store: Arc<MediaStore>,
    notifier: Arc<dyn ApprovalNotifier>,
}

impl ApprovalOrchestrator {
    pub fn new(store: Arc<MediaStore>, notifier: Arc<dyn ApprovalNotifier>) -> Self {
        Self { store, notifier }
    }

    #[instrument(skip(self, media_file), fields(id = ?media_file.id), err)]
    pub async fn send_request(&self, media_file: &MediaFile) -> Result<()> {
        let request = self.build_request(media_file).await?;

        match self
            .store
            .transition(
                &media_file.id,
                WorkflowStateTag::Analyzed,
                WorkflowStateTag::PendingApproval,
                &MediaEvent::PendingApproval,
            )
            .await
        {
            Ok(()) => {}
            Err(StoreError::StaleState { expected }) => {
                warn!(?expected, "approval already pending for this file");
            }
            Err(e) => return Err(e.into()),
        }
        self.publish_request(&request).await?;

        Ok(())
    }

    pub async fn resend_request(&self, media_file: &MediaFile) -> Result<()> {
        let request = self.build_request(media_file).await?;
        self.publish_request(&request).await
    }

    async fn build_request(&self, media_file: &MediaFile) -> Result<ApprovalRequest> {
        let info = self.store.fetch_approval_info(&media_file.id).await?;

        let Some(vp) = &media_file.video_properties else {
            bail!("missing video properties for {:?}", media_file.id);
        };

        let Some(crf) = info.crf else {
            bail!(
                "missing crf in transcode_spec for analyzed file {:?}",
                media_file.id
            );
        };
        let Some(compression_potential) = info.compression_potential else {
            bail!(
                "missing compression_potential in transcode_spec for analyzed file {:?}",
                media_file.id
            );
        };

        Ok(ApprovalRequest {
            media_file_id: media_file.id.as_uuid(),
            title: info
                .title
                .unwrap_or_else(|| media_file.path.as_ref().to_string_lossy().into_owned()),
            path: media_file.path.as_ref().to_string_lossy().into_owned(),
            codec: vp.video_codec.as_ref().to_string(),
            resolution: format!("{}x{}", vp.resolution.width(), vp.resolution.height()),
            size_gb: vp.size_bytes.as_gb(),
            compression_potential,
            crf: crf as u8,
            tmdb_rating: info.tmdb_rating,
        })
    }

    async fn publish_request(&self, request: &ApprovalRequest) -> Result<()> {
        self.notifier
            .request_approval(request)
            .await
            .map_err(Into::into)
    }

    #[instrument(skip(self), fields(media_file_id = %response.media_file_id, approved = response.approved), err)]
    async fn handle_response(&self, response: ApprovalResponse) -> Result<()> {
        let file_id = MediaFileId::from(response.media_file_id);
        if response.approved {
            match self
                .store
                .transition(
                    &file_id,
                    WorkflowStateTag::PendingApproval,
                    WorkflowStateTag::Transcoding,
                    &MediaEvent::ApprovalGranted,
                )
                .await
            {
                Ok(()) => {}
                Err(StoreError::StaleState { expected }) => {
                    warn!(?expected, "approval grant already processed");
                }
                Err(e) => return Err(e.into()),
            }
        } else {
            match self
                .store
                .transition(
                    &file_id,
                    WorkflowStateTag::PendingApproval,
                    WorkflowStateTag::Skipped,
                    &MediaEvent::ApprovalRejected,
                )
                .await
            {
                Ok(()) => {}
                Err(StoreError::StaleState { expected }) => {
                    warn!(?expected, "approval rejection already processed");
                }
                Err(e) => return Err(e.into()),
            }
        }
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
                    let ids = self
                        .store
                        .fetch_stale_pending_approvals(threshold_minutes as i32)
                        .await?;

                    for id in ids {
                        let Ok(media_file) = self.store.find_media_file_by_id(&id).await else {
                            continue;
                        };
                        if let Err(e) = self.resend_request(&media_file).await {
                            error!(?id, %e, "failed to resend approval request");
                        }
                    }
                }
            }
        }
    }
}
