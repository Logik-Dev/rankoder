mod decision;

use std::sync::Arc;

use anyhow::Result;
use tracing::{instrument, warn};

use crate::{
    models::media_file::MediaFile,
    store::{MediaStore, error::StoreError},
};

pub use decision::TakeTranscodeDecisionService;

#[derive(Clone)]
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

        match self
            .store
            .save_analysis_result(&media_file.id, &decision)
            .await
        {
            Ok(()) => {}
            Err(StoreError::StaleState { expected }) => {
                warn!(?expected, "analysis result already saved by another worker");
            }
            Err(e) => return Err(e.into()),
        }

        Ok(())
    }
}
