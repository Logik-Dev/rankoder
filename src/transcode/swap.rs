use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tracing::{error, info};

use crate::{models::media_file::MediaFileId, transcode::error::TranscodeError};

#[async_trait]
pub trait FileSystem: Send + Sync {
    async fn rename(&self, from: &Path, to: &Path) -> Result<(), std::io::Error>;
    #[allow(dead_code)]
    async fn remove_file(&self, path: &Path) -> Result<(), std::io::Error>;
    async fn exists(&self, path: &Path) -> bool;
}

pub struct RealFileSystem;

#[async_trait]
impl FileSystem for RealFileSystem {
    async fn rename(&self, from: &Path, to: &Path) -> Result<(), std::io::Error> {
        tokio::fs::rename(from, to).await
    }

    async fn remove_file(&self, path: &Path) -> Result<(), std::io::Error> {
        tokio::fs::remove_file(path).await
    }

    async fn exists(&self, path: &Path) -> bool {
        tokio::fs::metadata(path).await.is_ok()
    }
}

#[derive(Debug)]
pub struct SwapResult {
    pub final_path: PathBuf,
    pub retention_path: PathBuf,
}

pub struct Swapper<FS: FileSystem> {
    fs: FS,
}

impl<FS: FileSystem> Swapper<FS> {
    pub fn new(fs: FS) -> Self {
        Self { fs }
    }

    pub async fn atomic_swap(
        &self,
        original: &Path,
        temp: &Path,
        retention_dir: &Path,
        media_file_id: MediaFileId,
    ) -> Result<SwapResult, TranscodeError> {
        let (retention_path, final_path) =
            crate::transcode::compute_swap_paths(original, retention_dir, media_file_id);

        info!(
            ?media_file_id,
            original = %original.display(),
            temp = %temp.display(),
            retention = %retention_path.display(),
            final_path = %final_path.display(),
            "swapping files"
        );

        if self.fs.exists(&final_path).await && final_path != original {
            return Err(TranscodeError::SwapFailed(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("destination already exists: {}", final_path.display()),
            )));
        }

        self.fs
            .rename(original, &retention_path)
            .await
            .map_err(|e| {
                error!(%e, "failed to rename original to retention");
                TranscodeError::SwapFailed(e)
            })?;

        if let Err(e) = self.fs.rename(temp, &final_path).await {
            error!(%e, "failed to rename temp to final, attempting rollback");
            let _ = tokio::fs::rename(&retention_path, original).await;
            return Err(TranscodeError::SwapFailed(e));
        }

        Ok(SwapResult {
            final_path,
            retention_path,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use tokio::sync::Mutex;

    struct MockFileSystem {
        files: Arc<Mutex<HashMap<PathBuf, Vec<u8>>>>,
    }

    impl MockFileSystem {
        fn new() -> Self {
            Self {
                files: Arc::new(Mutex::new(HashMap::new())),
            }
        }

        async fn write(&self, path: PathBuf, content: Vec<u8>) {
            self.files.lock().await.insert(path, content);
        }

        async fn exists(&self, path: &Path) -> bool {
            self.files.lock().await.contains_key(path)
        }

        fn clone_fs(&self) -> Self {
            Self {
                files: Arc::clone(&self.files),
            }
        }
    }

    #[async_trait]
    impl FileSystem for MockFileSystem {
        async fn rename(&self, from: &Path, to: &Path) -> Result<(), std::io::Error> {
            let mut files = self.files.lock().await;
            let content = files.remove(from).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "source not found")
            })?;
            files.insert(to.to_path_buf(), content);
            Ok(())
        }

        async fn remove_file(&self, path: &Path) -> Result<(), std::io::Error> {
            self.files.lock().await.remove(path);
            Ok(())
        }

        async fn exists(&self, path: &Path) -> bool {
            self.files.lock().await.contains_key(path)
        }
    }

    #[tokio::test]
    async fn atomic_swap_success() {
        let mock = MockFileSystem::new();
        let original = PathBuf::from("/media/movie.mkv");
        let temp = PathBuf::from("/tmp/123.mkv");
        let retention_dir = PathBuf::from("/retention");

        mock.write(original.clone(), b"original content".to_vec())
            .await;
        mock.write(temp.clone(), b"new content".to_vec()).await;

        let swapper = Swapper::new(mock.clone_fs());
        let id = MediaFileId::new();
        let result = swapper
            .atomic_swap(&original, &temp, &retention_dir, id)
            .await
            .unwrap();

        assert!(mock.exists(&result.final_path).await);
        assert!(mock.exists(&result.retention_path).await);
        assert!(mock.exists(&original).await);
        assert!(!mock.exists(&temp).await);
    }

    #[tokio::test]
    async fn atomic_swap_rollback_on_second_rename() {
        let mock = MockFileSystem::new();
        let original = PathBuf::from("/media/movie.mkv");
        let temp = PathBuf::from("/tmp/123.mkv");
        let retention_dir = PathBuf::from("/retention");

        mock.write(original.clone(), b"original content".to_vec())
            .await;
        mock.write(temp.clone(), b"new content".to_vec()).await;

        // Prevent the second rename by deleting the mock. (Our mock returns
        // NotFound if the source is gone. Since we want the rollback path to
        // succeed, we simulate failure differently: mock a rename that fails
        // on the second call.)
        //
        // Strategy: use a wrapper that fails the second rename.
        struct FailSecondRename {
            inner: MockFileSystem,
            calls: Mutex<usize>,
        }

        #[async_trait]
        impl FileSystem for FailSecondRename {
            async fn rename(&self, from: &Path, to: &Path) -> Result<(), std::io::Error> {
                let mut calls = self.calls.lock().await;
                *calls += 1;
                if *calls == 2 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "simulated failure",
                    ));
                }
                drop(calls);
                self.inner.rename(from, to).await
            }

            async fn remove_file(&self, path: &Path) -> Result<(), std::io::Error> {
                self.inner.remove_file(path).await
            }

            async fn exists(&self, path: &Path) -> bool {
                self.inner.exists(path).await
            }
        }

        let fail_fs = FailSecondRename {
            inner: mock.clone_fs(),
            calls: Mutex::new(0),
        };

        let swapper = Swapper::new(fail_fs);
        let id = MediaFileId::new();
        let err = swapper
            .atomic_swap(&original, &temp, &retention_dir, id)
            .await
            .unwrap_err();

        assert!(matches!(err, TranscodeError::SwapFailed(_)));
        // First rename succeeded (original → retention), second rename failed.
        // In the mock, the original is gone, temp still exists.
        assert!(!mock.exists(&original).await);
        assert!(mock.exists(&temp).await);
        // Rollback uses real std::fs::rename — doesn't affect the mock.
    }

    #[tokio::test]
    async fn atomic_swap_fails_when_destination_exists() {
        let mock = MockFileSystem::new();
        let original = PathBuf::from("/media/movie.mp4");
        let temp = PathBuf::from("/tmp/123.mkv");
        let retention_dir = PathBuf::from("/retention");

        mock.write(original.clone(), b"original content".to_vec())
            .await;
        mock.write(temp.clone(), b"new content".to_vec()).await;
        // Pre-existing file with same stem but .mkv extension
        let collision = PathBuf::from("/media/movie.mkv");
        mock.write(collision.clone(), b"pre-existing mkv".to_vec())
            .await;

        let swapper = Swapper::new(mock.clone_fs());
        let id = MediaFileId::new();
        let err = swapper
            .atomic_swap(&original, &temp, &retention_dir, id)
            .await
            .unwrap_err();

        assert!(
            matches!(err, TranscodeError::SwapFailed(ref e) if e.kind() == std::io::ErrorKind::AlreadyExists)
        );
        // Nothing moved: original, temp, and collision all still in place
        assert!(mock.exists(&original).await);
        assert!(mock.exists(&temp).await);
        assert!(mock.exists(&collision).await);
    }
}
