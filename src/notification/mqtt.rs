use std::time::Duration;

use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use tokio::sync::{Mutex, mpsc};
use tracing::warn;

use super::{ApprovalNotifier, ApprovalRequest, ApprovalResponse, NotifierError};

const REQUEST_TOPIC: &str = "rankoder/approval/request";
const RESPONSE_TOPIC: &str = "rankoder/approval/response";

pub struct MqttNotifier {
    client: AsyncClient,
    // Mutex is required because `ApprovalNotifier::listen_responses` takes
    // `&self` (async_trait constraint), but `mpsc::Receiver::recv` needs
    // `&mut self`. In practice this lock is uncontended: `listen_responses`
    // is called exactly once per notifier lifetime, and the lock is held for
    // the entire duration.
    internal_rx: Mutex<mpsc::Receiver<ApprovalResponse>>,
}

impl MqttNotifier {
    pub fn new(host: &str, port: u16, client_id: &str) -> Self {
        let mut options = MqttOptions::new(client_id, host, port);
        options.set_keep_alive(Duration::from_secs(30));
        let (client, mut eventloop) = AsyncClient::new(options, 100);

        let (internal_tx, internal_rx) = mpsc::channel(100);

        let driver_client = client.clone();

        tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Packet::ConnAck(_))) => {
                        if let Err(e) = driver_client
                            .subscribe(RESPONSE_TOPIC, QoS::AtLeastOnce)
                            .await
                        {
                            warn!("failed to resubscribe on ConnAck: {e}");
                        }
                    }
                    Ok(Event::Incoming(Packet::Publish(p))) if p.topic == RESPONSE_TOPIC => {
                        match serde_json::from_slice::<ApprovalResponse>(&p.payload) {
                            Ok(response) => {
                                if internal_tx.send(response).await.is_err() {
                                    return;
                                }
                            }
                            Err(e) => warn!("ignoring malformed approval response: {e}"),
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("MQTT connection error: {e}");
                    }
                }
            }
        });

        Self {
            client,
            internal_rx: Mutex::new(internal_rx),
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
        let mut rx = self.internal_rx.lock().await;
        while let Some(response) = rx.recv().await {
            if tx.send(response).await.is_err() {
                return Ok(());
            }
        }
        Ok(())
    }
}
