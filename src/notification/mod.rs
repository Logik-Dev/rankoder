mod error;
pub mod mqtt;

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
