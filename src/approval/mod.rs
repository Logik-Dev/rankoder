use std::sync::Arc;

use anyhow::{Result, bail};
use tokio::sync::mpsc;
use tracing::{error, instrument, warn};

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
        let info = self.store.fetch_approval_info(&media_file.id).await?;

        let Some(vp) = &media_file.video_properties else {
            bail!("missing video properties for {:?}", media_file.id);
        };

        let request = ApprovalRequest {
            media_file_id: media_file.id.as_uuid(),
            title: info
                .title
                .unwrap_or_else(|| media_file.path.as_ref().to_string_lossy().into_owned()),
            path: media_file.path.as_ref().to_string_lossy().into_owned(),
            codec: vp.video_codec.as_ref().to_string(),
            resolution: format!("{}x{}", vp.resolution.width(), vp.resolution.height()),
            size_gb: vp.size_bytes.as_gb(),
            compression_potential: info.compression_potential.unwrap_or(0.0),
            crf: info.crf.unwrap_or(24) as u8,
            tmdb_rating: info.tmdb_rating,
        };

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
        self.notifier.request_approval(&request).await?;

        Ok(())
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

    pub async fn run_response_listener(self: Arc<Self>) -> Result<()> {
        let (tx, mut rx) = mpsc::channel::<ApprovalResponse>(32);
        let notifier = Arc::clone(&self.notifier);

        tokio::spawn(async move {
            if let Err(e) = notifier.listen_responses(tx).await {
                error!("MQTT response listener stopped: {e}");
            }
        });

        while let Some(response) = rx.recv().await {
            if let Err(e) = self.handle_response(response).await {
                error!("failed to handle approval response: {e}");
            }
        }

        Ok(())
    }
}
