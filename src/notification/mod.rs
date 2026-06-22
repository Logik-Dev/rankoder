mod error;
pub mod mqtt;
pub mod reporter;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

pub use error::NotifierError;

#[derive(Debug, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub batch_id: String,
    pub title: String,
    pub file_count: u32,
    pub total_size_gb: f64,
    pub total_space_saved_gb: f64,
    pub tmdb_rating: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub batch_id: String,
    pub approved: bool,
}

#[async_trait]
pub trait ApprovalNotifier: Send + Sync {
    async fn request_approval(&self, request: &ApprovalRequest) -> Result<(), NotifierError>;
    async fn listen_responses(
        &self,
        tx: mpsc::Sender<ApprovalResponse>,
    ) -> Result<(), NotifierError>;
}

/// A single transcode failure, surfaced immediately to the operator.
#[derive(Debug, Serialize)]
pub struct FailureAlert {
    pub media_file_id: String,
    pub kind: String,
    pub title: Option<String>,
    pub reason: String,
}

/// A point-in-time snapshot of the pipeline, published as a retained sensor so
/// the current state survives restarts and is always queryable.
#[derive(Debug, Default, Serialize)]
pub struct StatusSnapshot {
    /// Running rankoder version (`CARGO_PKG_VERSION`), so the deployed build is
    /// visible from Home Assistant without shelling into the host.
    pub version: String,
    pub discovered: i64,
    pub probed: i64,
    pub analyzed: i64,
    pub pending_approval: i64,
    pub transcoding: i64,
    pub done: i64,
    pub skipped: i64,
    pub failed: i64,
    pub space_saved_gb: f64,
    pub last_failure: Option<FailureAlert>,
}

#[async_trait]
pub trait StatusNotifier: Send + Sync {
    /// Push an immediate, non-retained alert about a single failure.
    async fn publish_failure(&self, alert: &FailureAlert) -> Result<(), NotifierError>;
    /// Publish the retained status snapshot.
    async fn publish_status(&self, status: &StatusSnapshot) -> Result<(), NotifierError>;
}
