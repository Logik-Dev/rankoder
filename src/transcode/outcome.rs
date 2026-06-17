use std::path::PathBuf;

use crate::models::{
    common::AbsoluteFilePath, media_file::SizeBytes, transcode::SkipReason, video::Bitrate,
};

/// Business outcome of a transcode attempt, before any store-side state
/// transition is applied.
#[derive(Debug)]
pub enum TranscodeOutcome {
    Completed(CompletedTranscode),
    Skipped {
        reason: SkipReason,
        /// Measured VMAF when the skip happened at the quality gate
        /// (`QualityTooLow`); `None` for skips decided before measurement.
        vmaf: Option<f64>,
    },
    /// Reserved: recovery determined that the DB already reflects a completed
    /// transcode, so no store update is required.
    #[allow(dead_code)]
    AlreadyRecovered,
}

#[derive(Debug)]
pub struct CompletedTranscode {
    pub final_path: AbsoluteFilePath,
    pub original_size: SizeBytes,
    pub new_size: SizeBytes,
    pub bitrate: Option<Bitrate>,
    pub retention_path: PathBuf,
    /// Measured VMAF of the accepted encode; `None` if measurement failed or
    /// the result came from crash recovery (not re-measured).
    pub vmaf: Option<f64>,
}
