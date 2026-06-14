use tokio::process::Command;

use crate::transcode::encoder::Encoder;
use crate::transcode::error::DetectError;

pub async fn detect_encoder(override_enc: Option<Encoder>) -> Result<Encoder, DetectError> {
    if let Some(enc) = override_enc {
        return Ok(enc);
    }

    for enc in [Encoder::Nvenc, Encoder::VideoToolbox, Encoder::Libx265] {
        if test_encoder(enc).await? {
            return Ok(enc);
        }
    }

    Err(DetectError::NoEncoderAvailable)
}

async fn test_encoder(enc: Encoder) -> Result<bool, DetectError> {
    let codec = match enc {
        Encoder::Nvenc => "hevc_nvenc",
        Encoder::VideoToolbox => "hevc_videotoolbox",
        Encoder::Libx265 => "libx265",
    };

    let output = Command::new("ffmpeg")
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=0.1",
            "-c:v",
            codec,
            "-f",
            "null",
            "-",
        ])
        .output()
        .await
        .map_err(DetectError::FfmpegSpawn)?;

    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn override_shortcircuits_detection() {
        let result = detect_encoder(Some(Encoder::Libx265)).await;
        assert_eq!(result.unwrap(), Encoder::Libx265);
    }

    #[tokio::test]
    async fn auto_falls_back_to_detection() {
        let result = detect_encoder(None).await;
        assert!(result.is_ok());
    }
}
