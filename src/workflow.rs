use std::{collections::VecDeque, sync::Arc};

use anyhow::Result;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, warn};

use crate::{
    analysis::AnalysisOrchestrator,
    approval::ApprovalOrchestrator,
    models::{event::MediaEvent, media_file::MediaFileId, workflow::WorkflowStateTag},
    probe::Prober,
    store::{MediaStore, error::StoreError},
};

pub struct WorkflowOrchestrator {
    rx: mpsc::Receiver<MediaFileId>,
    media_store: Arc<MediaStore>,
    prober: Arc<dyn Prober>,
    analysis_orchestrator: AnalysisOrchestrator,
    approval_orchestrator: Arc<ApprovalOrchestrator>,
    transcode_tx: mpsc::Sender<MediaFileId>,
}

impl WorkflowOrchestrator {
    pub fn new(
        rx: mpsc::Receiver<MediaFileId>,
        media_store: Arc<MediaStore>,
        prober: Arc<dyn Prober>,
        analysis_orchestrator: AnalysisOrchestrator,
        approval_orchestrator: Arc<ApprovalOrchestrator>,
        transcode_tx: mpsc::Sender<MediaFileId>,
    ) -> Self {
        Self {
            rx,
            media_store,
            prober,
            analysis_orchestrator,
            approval_orchestrator,
            transcode_tx,
        }
    }

    #[instrument(skip(self), err)]
    pub async fn run(self, token: CancellationToken) -> anyhow::Result<()> {
        let concurrency = std::thread::available_parallelism()?.get();
        info!(concurrency, "starting workflow orchestrator");

        let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
        let mut join_set = JoinSet::new();

        let store = self.media_store;
        let prober = self.prober;
        let analysis = self.analysis_orchestrator;
        let approval = self.approval_orchestrator;
        let transcode_tx = self.transcode_tx;
        let mut rx = self.rx;

        let mut pending = VecDeque::new();

        loop {
            tokio::select! {
                biased;
                _ = token.cancelled() => {
                    info!("workflow cancelled, draining remaining tasks");
                    break;
                }
                Some(media_file_id) = rx.recv() => {
                    pending.push_back(media_file_id);
                }
                permit = semaphore.clone().acquire_owned(), if !pending.is_empty() => {
                    let media_file_id = pending.pop_front().unwrap();
                    let _permit = permit.expect("semaphore closed");
                    let s = Arc::clone(&store);
                    let p = Arc::clone(&prober);
                    let a = analysis.clone();
                    let ap = Arc::clone(&approval);
                    let tt = transcode_tx.clone();

                    join_set.spawn(async move {
                        if let Err(e) = Self::process_file(s, p, a, ap, tt, media_file_id).await {
                            error!(%e, "failed to process file");
                        }
                    });
                }
                Some(res) = join_set.join_next() => {
                    if let Err(e) = res {
                        error!("worker task panicked: {e}");
                    }
                }
                else => break,
            }
        }

        while let Some(res) = join_set.join_next().await {
            if let Err(e) = res {
                error!("worker task panicked: {e}");
            }
        }

        info!("event channel closed, shutting down workflow orchestrator");
        Ok(())
    }

    #[instrument(skip(store, prober, analysis, approval, transcode_tx), fields(id = ?media_file_id), err)]
    async fn process_file(
        store: Arc<MediaStore>,
        prober: Arc<dyn Prober>,
        analysis: AnalysisOrchestrator,
        approval: Arc<ApprovalOrchestrator>,
        transcode_tx: mpsc::Sender<MediaFileId>,
        media_file_id: MediaFileId,
    ) -> Result<()> {
        let Ok(media_file) = store.find_media_file_by_id(&media_file_id).await else {
            error!(?media_file_id, "failed to find media file on database");
            return Ok(());
        };

        match media_file.workflow_state {
            WorkflowStateTag::Discovered => {
                let video_properties = match prober.probe(&media_file.path).await {
                    Ok(v) => v,
                    Err(error) => {
                        warn!(?media_file_id, %error, "failed to probe media file");
                        match store
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
                        return Ok(());
                    }
                };

                match store
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
                    }
                    Err(error) => {
                        error!(%error, ?media_file_id, "failed to save probe data");
                    }
                }
            }
            WorkflowStateTag::Probed => {
                if let Err(error) = analysis.analyze(&media_file).await {
                    error!(%error, ?media_file_id, "analysis failed");
                }
            }
            WorkflowStateTag::Analyzed
            | WorkflowStateTag::Done
            | WorkflowStateTag::Skipped
            | WorkflowStateTag::Failed => {
                approval.wake_feeder();
            }

            // no op
            WorkflowStateTag::PendingApproval => {}

            WorkflowStateTag::Transcoding => {
                if let Err(e) = transcode_tx.send(media_file_id).await {
                    error!(?media_file_id, "failed to send to transcode channel: {e}");
                }
            }
        }

        Ok(())
    }
}
