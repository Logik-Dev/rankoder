use std::path::Path;

use serde::Deserialize;
use tokio::process::Command;

use crate::{
    models::{
        media_file::SizeBytes,
        video::{Bitrate, DurationSecs},
    },
    transcode::error::ValidationError,
};

#[derive(Debug)]
pub struct ValidatedOutput {
    pub size_bytes: SizeBytes,
    pub bitrate: Option<Bitrate>,
}

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
    bit_rate: Option<String>,
    #[serde(default)]
    disposition: RawDisposition,
}

impl RawStream {
    fn is_attached_pic(&self) -> bool {
        self.disposition.attached_pic == Some(1)
    }
}

#[derive(Debug, Deserialize, Default)]
struct RawDisposition {
    #[serde(default)]
    attached_pic: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct RawFormat {
    duration: Option<String>,
    size: Option<String>,
    bit_rate: Option<String>,
}

pub async fn validate_output(
    temp_path: &Path,
    original_duration: Option<f64>,
    tolerance_secs: f64,
) -> Result<ValidatedOutput, ValidationError> {
    if tokio::fs::metadata(temp_path).await.is_err() {
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

    parse_probe_output(&output.stdout, original_duration, tolerance_secs)
}

fn parse_probe_output(
    stdout: &[u8],
    original_duration: Option<f64>,
    tolerance_secs: f64,
) -> Result<ValidatedOutput, ValidationError> {
    let raw: RawProbe = serde_json::from_slice(stdout)
        .map_err(|e| ValidationError::FfprobeFailed(format!("invalid ffprobe JSON: {e}")))?;

    let video_stream = raw
        .streams
        .iter()
        .find(|s| s.codec_type == "video" && !s.is_attached_pic())
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

    let bitrate = video_stream
        .bit_rate
        .as_deref()
        .or(format.bit_rate.as_deref())
        .and_then(|s| s.parse::<u64>().ok())
        .and_then(|b| Bitrate::new(b).ok());

    if let Some(orig_dur) = original_duration
        && let Some(new_dur) = duration.as_ref().map(|d| d.as_secs_f64())
        && (orig_dur - new_dur).abs() > tolerance_secs
    {
        return Err(ValidationError::DurationMismatch {
            original: orig_dur,
            new: new_dur,
        });
    }

    Ok(ValidatedOutput {
        size_bytes,
        bitrate,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_hevc_probe() {
        let json = r#"{
            "streams": [
                {"codec_type": "video", "codec_name": "hevc"}
            ],
            "format": {
                "duration": "3600.000000",
                "size": "500000000"
            }
        }"#;

        let result = parse_probe_output(json.as_bytes(), Some(3600.0), 1.0).unwrap();
        assert_eq!(result.size_bytes.as_u64(), 500_000_000);
    }

    #[test]
    fn invalid_codec_h264() {
        let json = r#"{
            "streams": [
                {"codec_type": "video", "codec_name": "h264"}
            ],
            "format": {
                "duration": "3600.000000",
                "size": "500000000"
            }
        }"#;

        let err = parse_probe_output(json.as_bytes(), Some(3600.0), 1.0).unwrap_err();
        assert!(matches!(err, ValidationError::WrongCodec));
    }

    #[test]
    fn duration_mismatch() {
        let json = r#"{
            "streams": [
                {"codec_type": "video", "codec_name": "hevc"}
            ],
            "format": {
                "duration": "3000.000000",
                "size": "500000000"
            }
        }"#;

        let err = parse_probe_output(json.as_bytes(), Some(3600.0), 1.0).unwrap_err();
        assert!(matches!(err, ValidationError::DurationMismatch { .. }));
    }

    #[test]
    fn missing_size_bytes() {
        let json = r#"{
            "streams": [
                {"codec_type": "video", "codec_name": "hevc"}
            ],
            "format": {
                "duration": "3600.000000"
            }
        }"#;

        let err = parse_probe_output(json.as_bytes(), Some(3600.0), 1.0).unwrap_err();
        assert!(matches!(err, ValidationError::FfprobeFailed(_)));
    }

    #[test]
    fn duration_within_tolerance_is_ok() {
        let json = r#"{
            "streams": [
                {"codec_type": "video", "codec_name": "hevc"}
            ],
            "format": {
                "duration": "3600.500000",
                "size": "500000000"
            }
        }"#;

        let result = parse_probe_output(json.as_bytes(), Some(3600.0), 1.0).unwrap();
        assert_eq!(result.size_bytes.as_u64(), 500_000_000);
    }

    #[test]
    fn no_original_duration_skips_check() {
        let json = r#"{
            "streams": [
                {"codec_type": "video", "codec_name": "hevc"}
            ],
            "format": {
                "duration": "3000.000000",
                "size": "500000000"
            }
        }"#;

        let result = parse_probe_output(json.as_bytes(), None, 1.0).unwrap();
        assert_eq!(result.size_bytes.as_u64(), 500_000_000);
    }

    #[test]
    fn parses_bitrate_from_stream() {
        let json = r#"{
            "streams": [
                {"codec_type": "video", "codec_name": "hevc", "bit_rate": "5000000"}
            ],
            "format": {
                "duration": "3600.000000",
                "size": "500000000"
            }
        }"#;

        let result = parse_probe_output(json.as_bytes(), Some(3600.0), 1.0).unwrap();
        assert_eq!(result.bitrate.unwrap().as_bps(), 5_000_000);
    }

    #[test]
    fn parses_bitrate_from_format_fallback() {
        let json = r#"{
            "streams": [
                {"codec_type": "video", "codec_name": "hevc"}
            ],
            "format": {
                "duration": "3600.000000",
                "size": "500000000",
                "bit_rate": "6000000"
            }
        }"#;

        let result = parse_probe_output(json.as_bytes(), Some(3600.0), 1.0).unwrap();
        assert_eq!(result.bitrate.unwrap().as_bps(), 6_000_000);
    }

    #[test]
    fn bitrate_none_when_absent() {
        let json = r#"{
            "streams": [
                {"codec_type": "video", "codec_name": "hevc"}
            ],
            "format": {
                "duration": "3600.000000",
                "size": "500000000"
            }
        }"#;

        let result = parse_probe_output(json.as_bytes(), Some(3600.0), 1.0).unwrap();
        assert!(result.bitrate.is_none());
    }

    #[test]
    fn attached_pic_stream_is_ignored() {
        let json = r#"{
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "mjpeg",
                    "disposition": { "attached_pic": 1 }
                },
                {
                    "codec_type": "video",
                    "codec_name": "hevc",
                    "disposition": { "attached_pic": 0 }
                }
            ],
            "format": {
                "duration": "3600.000000",
                "size": "500000000"
            }
        }"#;

        let result = parse_probe_output(json.as_bytes(), Some(3600.0), 1.0).unwrap();
        assert_eq!(result.size_bytes.as_u64(), 500_000_000);
    }

    #[test]
    fn only_attached_pic_stream_fails() {
        let json = r#"{
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "mjpeg",
                    "disposition": { "attached_pic": 1 }
                }
            ],
            "format": {
                "duration": "3600.000000",
                "size": "500000000"
            }
        }"#;

        let err = parse_probe_output(json.as_bytes(), Some(3600.0), 1.0).unwrap_err();
        assert!(matches!(err, ValidationError::FfprobeFailed(_)));
    }
}
