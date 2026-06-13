mod error;
pub mod mqtt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use uuid::Uuid;

pub use error::NotifierError;

#[derive(Debug, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub media_file_id: Uuid,
    pub title: String,
    pub size_gb: f64,
    pub estimated_size_gb: f64,
    pub space_saved_gb: f64,
    pub compression_potential: f64,
    pub tmdb_rating: Option<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub media_file_id: Uuid,
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
