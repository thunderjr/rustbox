use std::fs::File;
use std::path::Path;

use crate::error::{Result, StorageError};

/// Archive a directory into a tar.zstd compressed file.
pub fn archive_overlay(source_dir: &Path, archive_path: &Path) -> Result<u64> {
    let file = File::create(archive_path)?;
    let encoder = zstd::Encoder::new(file, 3)
        .map_err(|e| StorageError::Archive(format!("zstd encoder: {e}")))?;
    let mut tar_builder = tar::Builder::new(encoder);
    tar_builder
        .append_dir_all(".", source_dir)
        .map_err(|e| StorageError::Archive(format!("tar append: {e}")))?;
    let encoder = tar_builder
        .into_inner()
        .map_err(|e| StorageError::Archive(format!("tar finish: {e}")))?;
    encoder
        .finish()
        .map_err(|e| StorageError::Archive(format!("zstd finish: {e}")))?;

    let metadata = std::fs::metadata(archive_path)?;
    Ok(metadata.len())
}

/// Restore an archive to a target directory.
pub fn restore_overlay(archive_path: &Path, target_dir: &Path) -> Result<()> {
    let file = File::open(archive_path)?;
    let decoder = zstd::Decoder::new(file)
        .map_err(|e| StorageError::Archive(format!("zstd decoder: {e}")))?;
    let mut archive = tar::Archive::new(decoder);
    std::fs::create_dir_all(target_dir)?;
    archive
        .unpack(target_dir)
        .map_err(|e| StorageError::Archive(format!("tar unpack: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn archive_and_restore_roundtrip() {
        let source = TempDir::new().unwrap();
        fs::write(source.path().join("hello.txt"), "hello world").unwrap();
        fs::write(source.path().join("data.bin"), vec![0u8; 256]).unwrap();

        let archive_dir = TempDir::new().unwrap();
        let archive_path = archive_dir.path().join("test.tar.zst");

        let size = archive_overlay(source.path(), &archive_path).unwrap();
        assert!(size > 0);

        let restore_dir = TempDir::new().unwrap();
        restore_overlay(&archive_path, restore_dir.path()).unwrap();

        let hello = fs::read_to_string(restore_dir.path().join("hello.txt")).unwrap();
        assert_eq!(hello, "hello world");

        let data = fs::read(restore_dir.path().join("data.bin")).unwrap();
        assert_eq!(data.len(), 256);
    }

    #[test]
    fn archive_preserves_nested_dirs() {
        let source = TempDir::new().unwrap();
        let nested = source.path().join("a").join("b").join("c");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("deep.txt"), "deep content").unwrap();
        fs::write(source.path().join("a").join("top.txt"), "top content").unwrap();

        let archive_dir = TempDir::new().unwrap();
        let archive_path = archive_dir.path().join("nested.tar.zst");

        archive_overlay(source.path(), &archive_path).unwrap();

        let restore_dir = TempDir::new().unwrap();
        restore_overlay(&archive_path, restore_dir.path()).unwrap();

        let deep = fs::read_to_string(restore_dir.path().join("a/b/c/deep.txt")).unwrap();
        assert_eq!(deep, "deep content");

        let top = fs::read_to_string(restore_dir.path().join("a/top.txt")).unwrap();
        assert_eq!(top, "top content");
    }
}
