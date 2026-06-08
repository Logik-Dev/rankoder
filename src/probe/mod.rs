use tokio::process::Command;

use crate::{
    models::{common::AbsoluteFilePath, video::VideoProperties},
    probe::{error::FfprobeError, output::FfprobeOutput},
};

mod error;
mod mapping;
mod output;

pub struct FFmpeg;

impl FFmpeg {
    pub async fn probe(file_path: &AbsoluteFilePath) -> Result<VideoProperties, FfprobeError> {
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
            return Err(FfprobeError::ProcessFailed(output.status.code()));
        }

        let ffprobe_output: FfprobeOutput =
            serde_json::from_slice(&output.stdout).map_err(FfprobeError::InvalidOutput)?;

        ffprobe_output.try_into()
    }
}
