use thiserror::Error;

#[derive(Debug, Error)]
pub enum NotifierError {
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("mqtt client error: {0}")]
    Client(String),
    #[error("mqtt connection error: {0}")]
    Connection(String),
}
