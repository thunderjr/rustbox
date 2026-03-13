use thiserror::Error;

#[derive(Error, Debug)]
pub enum RustboxError {
    #[error("sandbox not found: {0}")]
    SandboxNotFound(String),
    #[error("snapshot not found: {0}")]
    SnapshotNotFound(String),
    #[error("command not found: {0}")]
    CommandNotFound(String),
    #[error("sandbox not running: {0}")]
    SandboxNotRunning(String),
    #[error("vm backend error: {0}")]
    VmBackend(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("agent communication error: {0}")]
    AgentComm(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, RustboxError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_sandbox_not_found() {
        let e = RustboxError::SandboxNotFound("sb-123".to_string());
        assert!(e.to_string().contains("sandbox not found"));
        assert!(e.to_string().contains("sb-123"));
    }

    #[test]
    fn display_snapshot_not_found() {
        let e = RustboxError::SnapshotNotFound("snap-1".to_string());
        assert!(e.to_string().contains("snapshot not found"));
    }

    #[test]
    fn display_command_not_found() {
        let e = RustboxError::CommandNotFound("cmd-1".to_string());
        assert!(e.to_string().contains("command not found"));
    }

    #[test]
    fn display_sandbox_not_running() {
        let e = RustboxError::SandboxNotRunning("sb-456".to_string());
        assert!(e.to_string().contains("sandbox not running"));
    }

    #[test]
    fn display_vm_backend() {
        let e = RustboxError::VmBackend("qemu crashed".to_string());
        assert!(e.to_string().contains("vm backend error"));
    }

    #[test]
    fn display_timeout() {
        let e = RustboxError::Timeout("30s exceeded".to_string());
        assert!(e.to_string().contains("timeout"));
    }

    #[test]
    fn display_invalid_config() {
        let e = RustboxError::InvalidConfig("bad cpu".to_string());
        assert!(e.to_string().contains("invalid configuration"));
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let e: RustboxError = io_err.into();
        match &e {
            RustboxError::Io(_) => {}
            other => panic!("expected Io variant, got: {:?}", other),
        }
        // The transparent display should contain the original message
        assert!(e.to_string().contains("file missing"));
    }

    #[test]
    fn from_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let e: RustboxError = json_err.into();
        match &e {
            RustboxError::Json(_) => {}
            other => panic!("expected Json variant, got: {:?}", other),
        }
    }

    #[test]
    fn display_internal() {
        let e = RustboxError::Internal("unexpected".to_string());
        assert_eq!(e.to_string(), "unexpected");
    }
}
