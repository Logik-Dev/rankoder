use std::path::{Path, PathBuf};

use crate::models::media_file::MediaFileId;

pub mod detect;
pub mod encoder;
pub mod error;
pub mod orchestrator;
pub mod reaper;
pub mod recovery;
pub mod swap;
pub mod validation;

pub(crate) fn compute_swap_paths(
    original: &Path,
    retention_dir: &Path,
    media_file_id: MediaFileId,
) -> (PathBuf, PathBuf) {
    let filename = original
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");

    let retention_path = retention_dir.join(format!("{}_{filename}", media_file_id.as_uuid()));
    let final_path = original
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(
            "{}.mkv",
            original
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("output")
        ));

    (retention_path, final_path)
}
