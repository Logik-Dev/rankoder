use tokio::sync::mpsc;
use tracing::info;

use crate::listener::EventNotification;

pub struct WorkflowOrchestrator {
    rx: mpsc::Receiver<EventNotification>,
}

impl WorkflowOrchestrator {
    pub fn new(rx: mpsc::Receiver<EventNotification>) -> Self {
        Self { rx }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        while let Some(event) = self.rx.recv().await {
            info!(?event, "workflow event received");
        }
        info!("event channel closed, shutting down workflow orchestrator");
        Ok(())
    }
}
