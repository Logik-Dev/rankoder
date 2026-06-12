use std::time::Duration;

use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, EventLoop, MqttOptions, Packet, QoS};
use tokio::sync::{Mutex, mpsc};
use tracing::warn;

use super::{ApprovalNotifier, ApprovalRequest, ApprovalResponse, NotifierError};

const REQUEST_TOPIC: &str = "rankoder/approval/request";
const RESPONSE_TOPIC: &str = "rankoder/approval/response";

pub struct MqttNotifier {
    client: AsyncClient,
    eventloop: Mutex<EventLoop>,
}

impl MqttNotifier {
    pub fn new(host: &str, port: u16, client_id: &str) -> Self {
        let mut options = MqttOptions::new(client_id, host, port);
        options.set_keep_alive(Duration::from_secs(30));
        let (client, eventloop) = AsyncClient::new(options, 100);
        Self {
            client,
            eventloop: Mutex::new(eventloop),
        }
    }
}

#[async_trait]
impl ApprovalNotifier for MqttNotifier {
    async fn request_approval(&self, request: &ApprovalRequest) -> Result<(), NotifierError> {
        let payload = serde_json::to_vec(request)?;
        self.client
            .publish(REQUEST_TOPIC, QoS::AtLeastOnce, false, payload)
            .await
            .map_err(|e| NotifierError::Client(e.to_string()))
    }

    async fn listen_responses(
        &self,
        tx: mpsc::Sender<ApprovalResponse>,
    ) -> Result<(), NotifierError> {
        let mut eventloop = self.eventloop.lock().await;
        self.client
            .subscribe(RESPONSE_TOPIC, QoS::AtLeastOnce)
            .await
            .map_err(|e| NotifierError::Client(e.to_string()))?;

        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::Publish(p))) if p.topic == RESPONSE_TOPIC => {
                    match serde_json::from_slice::<ApprovalResponse>(&p.payload) {
                        Ok(response) => {
                            if tx.send(response).await.is_err() {
                                return Ok(());
                            }
                        }
                        Err(e) => warn!("ignoring malformed approval response: {e}"),
                    }
                }
                Ok(_) => {}
                Err(e) => return Err(NotifierError::Connection(e.to_string())),
            }
        }
    }
}
