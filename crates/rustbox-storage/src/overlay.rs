use std::path::{Path, PathBuf};

/// Configuration for an overlay filesystem mount for a single sandbox.
pub struct OverlayConfig {
    pub sandbox_id: String,
    base_dir: PathBuf,
}

impl OverlayConfig {
    pub fn new(sandbox_id: &str, base_dir: &Path) -> Self {
        Self {
            sandbox_id: sandbox_id.to_string(),
            base_dir: base_dir.to_path_buf(),
        }
    }

    /// The lower (read-only) directory for the base image.
    pub fn lower_dir(&self) -> PathBuf {
        self.base_dir.join("lower").join(&self.sandbox_id)
    }

    /// The upper (read-write) directory for sandbox modifications.
    pub fn upper_dir(&self) -> PathBuf {
        self.base_dir.join("upper").join(&self.sandbox_id)
    }

    /// The work directory required by overlayfs.
    pub fn work_dir(&self) -> PathBuf {
        self.base_dir.join("work").join(&self.sandbox_id)
    }

    /// The merged mount point.
    pub fn merged_dir(&self) -> PathBuf {
        self.base_dir.join("merged").join(&self.sandbox_id)
    }

    /// Generate the overlayfs mount options string.
    pub fn mount_options(&self) -> String {
        format!(
            "lowerdir={},upperdir={},workdir={}",
            self.lower_dir().display(),
            self.upper_dir().display(),
            self.work_dir().display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn correct_path_generation() {
        let base = Path::new("/tmp/overlay");
        let cfg = OverlayConfig::new("sandbox-1", base);

        assert_eq!(cfg.lower_dir(), PathBuf::from("/tmp/overlay/lower/sandbox-1"));
        assert_eq!(cfg.upper_dir(), PathBuf::from("/tmp/overlay/upper/sandbox-1"));
        assert_eq!(cfg.work_dir(), PathBuf::from("/tmp/overlay/work/sandbox-1"));
        assert_eq!(cfg.merged_dir(), PathBuf::from("/tmp/overlay/merged/sandbox-1"));
    }

    #[test]
    fn mount_options_format() {
        let base = Path::new("/mnt/store");
        let cfg = OverlayConfig::new("abc", base);
        let opts = cfg.mount_options();

        assert!(opts.contains("lowerdir="));
        assert!(opts.contains("upperdir="));
        assert!(opts.contains("workdir="));
        assert!(opts.contains("/mnt/store/lower/abc"));
        assert!(opts.contains("/mnt/store/upper/abc"));
        assert!(opts.contains("/mnt/store/work/abc"));
    }

    #[test]
    fn distinct_paths_for_different_sandbox_ids() {
        let base = Path::new("/tmp/overlay");
        let cfg_a = OverlayConfig::new("sandbox-a", base);
        let cfg_b = OverlayConfig::new("sandbox-b", base);

        assert_ne!(cfg_a.lower_dir(), cfg_b.lower_dir());
        assert_ne!(cfg_a.upper_dir(), cfg_b.upper_dir());
        assert_ne!(cfg_a.work_dir(), cfg_b.work_dir());
        assert_ne!(cfg_a.merged_dir(), cfg_b.merged_dir());
    }
}
