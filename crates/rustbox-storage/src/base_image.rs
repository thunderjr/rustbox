use rustbox_core::sandbox::Runtime;
use std::path::{Path, PathBuf};

use crate::error::{Result, StorageError};

/// Manages base VM images for each supported runtime.
pub struct BaseImageStore {
    images_dir: PathBuf,
}

impl BaseImageStore {
    pub fn new(images_dir: &Path) -> Self {
        Self {
            images_dir: images_dir.to_path_buf(),
        }
    }

    /// Get the path for a runtime's base image (ext4 filesystem).
    pub fn path_for_runtime(&self, runtime: &Runtime) -> PathBuf {
        let name = match runtime {
            Runtime::Node24 => "node24",
            Runtime::Node22 => "node22",
            Runtime::Python313 => "python313",
        };
        self.images_dir.join(format!("{name}.ext4"))
    }

    /// Check if a base image exists for the given runtime.
    pub fn exists(&self, runtime: &Runtime) -> bool {
        self.path_for_runtime(runtime).exists()
    }

    /// Get the path, returning an error if the image doesn't exist.
    pub fn require(&self, runtime: &Runtime) -> Result<PathBuf> {
        let path = self.path_for_runtime(runtime);
        if !path.exists() {
            return Err(StorageError::BaseImageNotFound(
                path.display().to_string(),
            ));
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn path_for_runtime_returns_correct_ext4_path() {
        let dir = TempDir::new().unwrap();
        let store = BaseImageStore::new(dir.path());

        assert_eq!(
            store.path_for_runtime(&Runtime::Node24),
            dir.path().join("node24.ext4")
        );
        assert_eq!(
            store.path_for_runtime(&Runtime::Node22),
            dir.path().join("node22.ext4")
        );
        assert_eq!(
            store.path_for_runtime(&Runtime::Python313),
            dir.path().join("python313.ext4")
        );
    }

    #[test]
    fn exists_returns_false_for_missing_image() {
        let dir = TempDir::new().unwrap();
        let store = BaseImageStore::new(dir.path());

        assert!(!store.exists(&Runtime::Node24));
        assert!(!store.exists(&Runtime::Node22));
        assert!(!store.exists(&Runtime::Python313));
    }

    #[test]
    fn require_returns_error_for_missing_image() {
        let dir = TempDir::new().unwrap();
        let store = BaseImageStore::new(dir.path());

        let err = store.require(&Runtime::Node24).unwrap_err();
        match err {
            StorageError::BaseImageNotFound(msg) => {
                assert!(msg.contains("node24.ext4"));
            }
            other => panic!("expected BaseImageNotFound, got: {other:?}"),
        }
    }
}
