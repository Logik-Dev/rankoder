use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::{error, info, instrument, warn};

use crate::{
    analysis::AnalysisOrchestrator,
    approval::ApprovalOrchestrator,
    models::{event::MediaEvent, media_file::MediaFileId, workflow::WorkflowStateTag},
    probe::FFmpeg,
    store::{MediaStore, error::StoreError},
};

pub struct WorkflowOrchestrator {
    rx: mpsc::Receiver<MediaFileId>,
    media_store: Arc<MediaStore>,
    _ffmpeg: FFmpeg,
    analysis_orchestrator: AnalysisOrchestrator,
    approval_orchestrator: Arc<ApprovalOrchestrator>,
}

impl WorkflowOrchestrator {
    pub fn new(
        rx: mpsc::Receiver<MediaFileId>,
        media_store: Arc<MediaStore>,
        ffmpeg: FFmpeg,
        analysis_orchestrator: AnalysisOrchestrator,
        approval_orchestrator: Arc<ApprovalOrchestrator>,
    ) -> Self {
        Self {
            rx,
            media_store,
            _ffmpeg: ffmpeg,
            analysis_orchestrator,
            approval_orchestrator,
        }
    }

    #[instrument(skip(self), err)]
    pub async fn run(mut self) -> anyhow::Result<()> {
        info!("starting workflow orchestrator");

        while let Some(media_file_id) = self.rx.recv().await {
            let Ok(media_file) = self.media_store.find_media_file_by_id(&media_file_id).await
            else {
                error!(?media_file_id, "failed to find media file on database");
                continue;
            };

            match media_file.workflow_state {
                WorkflowStateTag::Discovered => {
                    let video_properties = match FFmpeg::probe(&media_file.path).await {
                        Ok(v) => v,
                        Err(error) => {
                            warn!(?media_file_id, %error, "failed to probe media file");
                            match self
                                .media_store
                                .transition(
                                    &media_file_id,
                                    WorkflowStateTag::Discovered,
                                    WorkflowStateTag::Failed,
                                    &MediaEvent::ProbeFailed {
                                        error: error.to_string(),
                                    },
                                )
                                .await
                            {
                                Ok(()) => {}
                                Err(StoreError::StaleState { expected }) => {
                                    warn!(
                                        ?media_file_id,
                                        ?expected,
                                        "probe already processed by another worker"
                                    );
                                }
                                Err(e) => {
                                    error!(%e, ?media_file_id, "failed to save probe failure");
                                }
                            }
                            continue;
                        }
                    };

                    match self
                        .media_store
                        .insert_probe_data(&media_file_id, &video_properties)
                        .await
                    {
                        Ok(()) => {}
                        Err(StoreError::StaleState { expected }) => {
                            warn!(
                                ?media_file_id,
                                ?expected,
                                "probe data already inserted by another worker, skipping"
                            );
                            continue;
                        }
                        Err(error) => {
                            error!(%error, ?media_file_id, "failed to save probe data");
                            continue;
                        }
                    }
                }
                WorkflowStateTag::Probed => {
                    if let Err(error) = self.analysis_orchestrator.analyze(&media_file).await {
                        error!(%error, ?media_file_id, "analysis failed");
                    }
                }
                WorkflowStateTag::Analyzed => {
                    if let Err(error) = self.approval_orchestrator.send_request(&media_file).await {
                        error!(%error, ?media_file_id, "failed to send approval request");
                    }
                }
                WorkflowStateTag::PendingApproval
                | WorkflowStateTag::Transcoding
                | WorkflowStateTag::Done
                | WorkflowStateTag::Skipped
                | WorkflowStateTag::Failed => {}
            }
        }
        info!("event channel closed, shutting down workflow orchestrator");
        Ok(())
    }
}
