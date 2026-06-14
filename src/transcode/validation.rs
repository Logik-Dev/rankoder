use std::path::Path;

use serde::Deserialize;
use tokio::process::Command;

use crate::{
    models::{
        media_file::SizeBytes,
        video::{DurationSecs, VideoProperties},
    },
    transcode::error::ValidationError,
};

#[derive(Debug, Deserialize)]
struct RawProbe {
    streams: Vec<RawStream>,
    #[serde(default)]
    format: Option<RawFormat>,
}

#[derive(Debug, Deserialize)]
struct RawStream {
    codec_type: String,
    codec_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawFormat {
    duration: Option<String>,
    size: Option<String>,
}

pub async fn validate_output(
    temp_path: &Path,
    original_duration: Option<f64>,
    tolerance_secs: f64,
) -> Result<VideoProperties, ValidationError> {
    if !temp_path.exists() {
        return Err(ValidationError::FfprobeFailed(
            "output file does not exist".into(),
        ));
    }

    let output = Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(temp_path)
        .output()
        .await
        .map_err(|e| ValidationError::FfprobeFailed(e.to_string()))?;

    if !output.status.success() {
        return Err(ValidationError::FfprobeFailed(format!(
            "ffprobe exit code {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
        )));
    }

    let raw: RawProbe = serde_json::from_slice(&output.stdout)
        .map_err(|e| ValidationError::FfprobeFailed(format!("invalid ffprobe JSON: {e}")))?;

    let video_stream = raw
        .streams
        .iter()
        .find(|s| s.codec_type == "video")
        .ok_or_else(|| ValidationError::FfprobeFailed("no video stream".into()))?;

    let codec_name = video_stream.codec_name.as_deref().unwrap_or("unknown");
    if !matches!(codec_name, "hevc") {
        return Err(ValidationError::WrongCodec);
    }

    let format = raw
        .format
        .as_ref()
        .ok_or_else(|| ValidationError::FfprobeFailed("no format section".into()))?;

    let duration = format
        .duration
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok())
        .and_then(|d| DurationSecs::new(d).ok());

    let size_bytes = format
        .size
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
        .and_then(|b| SizeBytes::new(b).ok())
        .ok_or_else(|| ValidationError::FfprobeFailed("missing size bytes".into()))?;

    if let Some(orig_dur) = original_duration
        && let Some(new_dur) = duration.as_ref().map(|d| d.as_secs_f64())
        && (orig_dur - new_dur).abs() > tolerance_secs
    {
        return Err(ValidationError::DurationMismatch {
            original: orig_dur,
            new: new_dur,
        });
    }

    let video_codec: crate::models::video::VideoCodec = codec_name.parse().unwrap();
    let resolution = crate::models::video::Resolution::new(0, 0).unwrap();

    Ok(VideoProperties {
        video_codec,
        resolution,
        bitrate: None,
        framerate: None,
        size_bytes,
        duration,
    })
}
