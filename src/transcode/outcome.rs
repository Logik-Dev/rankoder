use std::path::PathBuf;

use crate::models::{
    common::AbsoluteFilePath, media_file::SizeBytes, transcode::SkipReason, video::Bitrate,
};

/// Business outcome of a transcode attempt, before any store-side state
/// transition is applied.
#[derive(Debug)]
#[allow(dead_code)]
pub enum TranscodeOutcome {
    Completed(CompletedTranscode),
    Skipped(SkipReason),
    /// Reserved: recovery determined that the DB already reflects a completed
    /// transcode, so no store update is required.
    #[allow(dead_code)]
    AlreadyRecovered,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct CompletedTranscode {
    pub final_path: AbsoluteFilePath,
    pub new_size: SizeBytes,
    pub bitrate: Option<Bitrate>,
    pub retention_path: PathBuf,
}
