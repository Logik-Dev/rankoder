use std::path::Path;

use serde::Deserialize;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum VmafError {
    #[error("failed to spawn ffmpeg for vmaf: {0}")]
    Spawn(std::io::Error),
    #[error("ffmpeg vmaf exited with {code:?}: {stderr}")]
    Ffmpeg { code: Option<i32>, stderr: String },
    #[error("failed to read vmaf log: {0}")]
    ReadLog(std::io::Error),
    #[error("failed to parse vmaf log: {0}")]
    Parse(serde_json::Error),
    #[error("vmaf log missing pooled mean score")]
    MissingScore,
}

#[derive(Deserialize)]
struct VmafLog {
    pooled_metrics: Option<PooledMetrics>,
}

#[derive(Deserialize)]
struct PooledMetrics {
    vmaf: VmafPooled,
}

#[derive(Deserialize)]
struct VmafPooled {
    mean: f64,
}

fn parse_vmaf_log(json: &str) -> Result<f64, VmafError> {
    let log: VmafLog = serde_json::from_str(json).map_err(VmafError::Parse)?;
    log.pooled_metrics
        .map(|m| m.vmaf.mean)
        .ok_or(VmafError::MissingScore)
}

/// Mean VMAF of `transcoded` (distorted) against `original` (reference).
///
/// Both inputs are assumed to share the same resolution — rankoder never
/// rescales — so no scaling is needed; we only align the pixel format so 8-bit
/// and 10-bit sources are comparable. `n_subsample` evaluates one frame out of
/// every N to bound the cost (1 = every frame).
pub async fn compute_vmaf(
    original: &Path,
    transcoded: &Path,
    n_subsample: u32,
) -> Result<f64, VmafError> {
    let n_subsample = n_subsample.max(1);
    let log_path = transcoded.with_extension("vmaf.json");

    // First input (transcoded) is the distorted stream, second (original) the
    // reference, as libvmaf expects.
    let filtergraph = format!(
        "[0:v]format=yuv420p10le[dist];[1:v]format=yuv420p10le[ref];\
         [dist][ref]libvmaf=log_fmt=json:log_path={}:n_subsample={}",
        log_path.display(),
        n_subsample,
    );

    let output = Command::new("ffmpeg")
        .arg("-nostdin")
        .arg("-i")
        .arg(transcoded)
        .arg("-i")
        .arg(original)
        .arg("-lavfi")
        .arg(&filtergraph)
        .arg("-f")
        .arg("null")
        .arg("-")
        .output()
        .await
        .map_err(VmafError::Spawn)?;

    if !output.status.success() {
        let _ = tokio::fs::remove_file(&log_path).await;
        return Err(VmafError::Ffmpeg {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }

    let json = tokio::fs::read_to_string(&log_path)
        .await
        .map_err(VmafError::ReadLog)?;
    let _ = tokio::fs::remove_file(&log_path).await;

    parse_vmaf_log(&json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pooled_mean() {
        let json = r#"{
            "frames": [],
            "pooled_metrics": { "vmaf": { "min": 88.1, "max": 99.2, "mean": 96.45, "harmonic_mean": 96.1 } }
        }"#;
        assert_eq!(parse_vmaf_log(json).unwrap(), 96.45);
    }

    #[test]
    fn missing_pooled_metrics_is_error() {
        let json = r#"{ "frames": [] }"#;
        assert!(matches!(
            parse_vmaf_log(json),
            Err(VmafError::MissingScore)
        ));
    }

    #[test]
    fn malformed_json_is_parse_error() {
        assert!(matches!(parse_vmaf_log("not json"), Err(VmafError::Parse(_))));
    }
}
