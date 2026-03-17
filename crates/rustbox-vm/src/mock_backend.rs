use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::mpsc;

use rustbox_core::{
    CommandId, CommandOutput, CommandRequest, Result, RustboxError,
    SandboxConfig, SandboxId, SandboxMetrics, SandboxStatus, SnapshotId,
    backend::VmBackend,
    network::NetworkPolicy,
};

/// A mock VM backend for testing that does not spawn real VMs.
pub struct MockBackend {
    sandboxes: DashMap<String, MockSandbox>,
}

struct MockSandbox {
    #[allow(dead_code)]
    config: SandboxConfig,
    status: SandboxStatus,
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            sandboxes: DashMap::new(),
        }
    }

    /// Retrieve the stored config for a sandbox (for testing).
    pub fn get_config(&self, id: &SandboxId) -> Option<SandboxConfig> {
        self.sandboxes.get(&id.to_string()).map(|s| s.config.clone())
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VmBackend for MockBackend {
    async fn create(&self, id: &SandboxId, config: &SandboxConfig) -> Result<()> {
        self.sandboxes.insert(
            id.to_string(),
            MockSandbox {
                config: config.clone(),
                status: SandboxStatus::Pending,
            },
        );
        Ok(())
    }

    async fn start(&self, id: &SandboxId) -> Result<()> {
        let key = id.to_string();
        let mut sandbox = self
            .sandboxes
            .get_mut(&key)
            .ok_or_else(|| RustboxError::SandboxNotFound(key))?;
        sandbox.status = SandboxStatus::Running;
        Ok(())
    }

    async fn stop(&self, id: &SandboxId, _blocking: bool) -> Result<()> {
        let key = id.to_string();
        let mut sandbox = self
            .sandboxes
            .get_mut(&key)
            .ok_or_else(|| RustboxError::SandboxNotFound(key))?;
        sandbox.status = SandboxStatus::Stopped;
        Ok(())
    }

    async fn status(&self, id: &SandboxId) -> Result<SandboxStatus> {
        let key = id.to_string();
        let sandbox = self
            .sandboxes
            .get(&key)
            .ok_or_else(|| RustboxError::SandboxNotFound(key))?;
        Ok(sandbox.status.clone())
    }

    async fn exec(
        &self,
        id: &SandboxId,
        _cmd: &CommandRequest,
    ) -> Result<(CommandId, mpsc::Receiver<CommandOutput>)> {
        let key = id.to_string();
        if !self.sandboxes.contains_key(&key) {
            return Err(RustboxError::SandboxNotFound(key));
        }

        let cmd_id = CommandId::new();
        let (tx, rx) = mpsc::channel(16);

        // Send mock output.
        tokio::spawn(async move {
            let _ = tx.send(CommandOutput::Stdout(b"mock output\n".to_vec())).await;
            let _ = tx.send(CommandOutput::Exit(0)).await;
        });

        Ok((cmd_id, rx))
    }

    async fn kill_command(
        &self,
        id: &SandboxId,
        _cmd_id: &CommandId,
        _signal: i32,
    ) -> Result<()> {
        let key = id.to_string();
        if !self.sandboxes.contains_key(&key) {
            return Err(RustboxError::SandboxNotFound(key));
        }
        Ok(())
    }

    async fn write_file(
        &self,
        id: &SandboxId,
        _path: &str,
        _content: &[u8],
    ) -> Result<()> {
        let key = id.to_string();
        if !self.sandboxes.contains_key(&key) {
            return Err(RustboxError::SandboxNotFound(key));
        }
        Ok(())
    }

    async fn read_file(&self, id: &SandboxId, _path: &str) -> Result<Vec<u8>> {
        let key = id.to_string();
        if !self.sandboxes.contains_key(&key) {
            return Err(RustboxError::SandboxNotFound(key));
        }
        Ok(b"mock file content".to_vec())
    }

    async fn mkdir(&self, id: &SandboxId, _path: &str) -> Result<()> {
        let key = id.to_string();
        if !self.sandboxes.contains_key(&key) {
            return Err(RustboxError::SandboxNotFound(key));
        }
        Ok(())
    }

    async fn update_network_policy(&self, id: &SandboxId, policy: &NetworkPolicy) -> Result<()> {
        let key = id.to_string();
        let mut sandbox = self
            .sandboxes
            .get_mut(&key)
            .ok_or_else(|| RustboxError::SandboxNotFound(key))?;
        sandbox.config.network_policy = policy.clone();
        Ok(())
    }

    async fn snapshot_create(&self, id: &SandboxId) -> Result<SnapshotId> {
        let key = id.to_string();
        if !self.sandboxes.contains_key(&key) {
            return Err(RustboxError::SandboxNotFound(key));
        }
        Ok(SnapshotId::new())
    }

    async fn snapshot_restore(
        &self,
        id: &SandboxId,
        _snap: &SnapshotId,
    ) -> Result<()> {
        let key = id.to_string();
        if !self.sandboxes.contains_key(&key) {
            return Err(RustboxError::SandboxNotFound(key));
        }
        Ok(())
    }

    async fn metrics(&self, id: &SandboxId) -> Result<SandboxMetrics> {
        let key = id.to_string();
        if !self.sandboxes.contains_key(&key) {
            return Err(RustboxError::SandboxNotFound(key));
        }
        Ok(SandboxMetrics::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustbox_core::sandbox::{CpuCount, Runtime, SandboxConfig};
    use rustbox_core::network::NetworkPolicy;
    use rustbox_core::command::CommandRequest;
    use std::collections::HashMap;
    use std::time::Duration;

    fn test_config() -> SandboxConfig {
        SandboxConfig {
            runtime: Runtime::Node24,
            cpu_count: CpuCount::One,
            timeout: Duration::from_secs(300),
            env: HashMap::new(),
            ports: vec![],
            network_policy: NetworkPolicy::default(),
            source: None,
        }
    }

    fn test_cmd() -> CommandRequest {
        CommandRequest {
            cmd: "echo".into(),
            args: vec!["hello".into()],
            cwd: None,
            env: None,
            sudo: false,
            detached: false,
        }
    }

    #[tokio::test]
    async fn create_then_status_pending() {
        let backend = MockBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        let status = backend.status(&id).await.unwrap();
        assert_eq!(status, SandboxStatus::Pending);
    }

    #[tokio::test]
    async fn start_then_running() {
        let backend = MockBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        backend.start(&id).await.unwrap();
        let status = backend.status(&id).await.unwrap();
        assert_eq!(status, SandboxStatus::Running);
    }

    #[tokio::test]
    async fn stop_then_stopped() {
        let backend = MockBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        backend.start(&id).await.unwrap();
        backend.stop(&id, false).await.unwrap();
        let status = backend.status(&id).await.unwrap();
        assert_eq!(status, SandboxStatus::Stopped);
    }

    #[tokio::test]
    async fn status_missing_sandbox() {
        let backend = MockBackend::new();
        let id = SandboxId::new();
        let result = backend.status(&id).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, RustboxError::SandboxNotFound(_)),
            "expected SandboxNotFound, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn exec_returns_output() {
        let backend = MockBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        backend.start(&id).await.unwrap();

        let (_cmd_id, mut rx) = backend.exec(&id, &test_cmd()).await.unwrap();

        let first = rx.recv().await.expect("should receive stdout");
        match &first {
            CommandOutput::Stdout(data) => assert_eq!(data, b"mock output\n"),
            other => panic!("expected Stdout, got: {other:?}"),
        }

        let second = rx.recv().await.expect("should receive exit");
        match &second {
            CommandOutput::Exit(code) => assert_eq!(*code, 0),
            other => panic!("expected Exit(0), got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_file_returns_mock_content() {
        let backend = MockBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        backend.start(&id).await.unwrap();

        let content = backend.read_file(&id, "/any/path").await.unwrap();
        assert_eq!(content, b"mock file content");
    }

    #[tokio::test]
    async fn snapshot_create_returns_id() {
        let backend = MockBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        backend.start(&id).await.unwrap();

        let snap_id = backend.snapshot_create(&id).await.unwrap();
        assert!(!snap_id.0.is_empty(), "SnapshotId should not be empty");
    }

    #[tokio::test]
    async fn update_network_policy_stores_new_policy() {
        let backend = MockBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        backend.start(&id).await.unwrap();

        let new_policy = NetworkPolicy {
            mode: rustbox_core::network::NetworkMode::DenyAll,
            allow_domains: vec!["example.com".to_string()],
            ..NetworkPolicy::default()
        };
        backend
            .update_network_policy(&id, &new_policy)
            .await
            .unwrap();

        let config = backend.get_config(&id).unwrap();
        assert!(matches!(
            config.network_policy.mode,
            rustbox_core::network::NetworkMode::DenyAll
        ));
        assert_eq!(config.network_policy.allow_domains, vec!["example.com"]);
    }

    #[tokio::test]
    async fn metrics_returns_default() {
        let backend = MockBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        backend.start(&id).await.unwrap();

        let metrics = backend.metrics(&id).await.unwrap();
        assert_eq!(metrics.cpu_usage_percent, 0.0);
        assert_eq!(metrics.memory_used_bytes, 0);
        assert_eq!(metrics.memory_total_bytes, 0);
        assert_eq!(metrics.network_rx_bytes, 0);
        assert_eq!(metrics.network_tx_bytes, 0);
        assert_eq!(metrics.disk_used_bytes, 0);
    }
}
