use crate::models::{error::DomainError, workflow::WorkflowStateTag};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("event serialization failed: {0}")]
    EventSerialization(#[from] serde_json::Error),
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error("stale state: expected {expected:?}, but row was already advanced")]
    StaleState { expected: WorkflowStateTag },
}
