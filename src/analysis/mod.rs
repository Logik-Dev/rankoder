mod decision;

use std::sync::Arc;

use anyhow::Result;
use tracing::{instrument, warn};

use crate::{
    models::{media_file::MediaFile, transcode::{SkipReason, TranscodeDecision}},
    store::MediaStore,
};

pub use decision::TakeTranscodeDecisionService;

pub struct AnalysisOrchestrator {
    store: Arc<MediaStore>,
    decision_service: TakeTranscodeDecisionService,
}

impl AnalysisOrchestrator {
    pub fn new(store: Arc<MediaStore>, decision_service: TakeTranscodeDecisionService) -> Self {
        Self {
            store,
            decision_service,
        }
    }

    #[instrument(skip(self, media_file), fields(id = ?media_file.id), err)]
    pub async fn analyze(&self, media_file: &MediaFile) -> Result<()> {
        let rating = self
            .store
            .fetch_tmdb_rating_for_file(&media_file.id)
            .await?;

        let decision = self.decision_service.execute(media_file, rating);

        // Guard skips mean the file is already in a non-probed state — no state change needed
        if let TranscodeDecision::Skip(
            SkipReason::TranscodeInProgress | SkipReason::AlreadyTranscoded,
        ) = &decision
        {
            warn!("analysis triggered on non-probed file, skipping state update");
            return Ok(());
        }

        self.store
            .save_analysis_result(&media_file.id, &decision)
            .await?;

        Ok(())
    }
}
