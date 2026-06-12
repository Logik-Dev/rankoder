use async_trait::async_trait;
use tokio::process::Command;

use crate::{
    models::{common::AbsoluteFilePath, video::VideoProperties},
    probe::{error::FfprobeError, output::FfprobeOutput},
};

mod error;
mod mapping;
mod output;

pub struct FFmpeg;

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("ffprobe error: {0}")]
    Ffprobe(#[from] FfprobeError),
}

#[async_trait]
pub trait Prober: Send + Sync {
    async fn probe(&self, path: &AbsoluteFilePath) -> Result<VideoProperties, ProbeError>;
}

#[async_trait]
impl Prober for FFmpeg {
    async fn probe(&self, file_path: &AbsoluteFilePath) -> Result<VideoProperties, ProbeError> {
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "quiet",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(file_path.as_ref())
            .output()
            .await
            .map_err(FfprobeError::SpawnFailed)?;

        if !output.status.success() {
            return Err(FfprobeError::ProcessFailed(output.status.code()).into());
        }

        let ffprobe_output: FfprobeOutput =
            serde_json::from_slice(&output.stdout).map_err(FfprobeError::InvalidOutput)?;

        ffprobe_output.try_into().map_err(Into::into)
    }
}
