use chrono::Utc;
use dashmap::DashMap;
use rustbox_core::{
    CommandId, CommandOutput, CommandRequest, Result, RustboxError,
    SandboxConfig, SandboxId, SandboxMetrics, SandboxStatus,
    backend::VmBackend,
    network::NetworkPolicy,
    sandbox::Sandbox,
};
use rustbox_storage::SnapshotStore;
use rustbox_storage::SnapshotMetadata;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

pub struct SandboxEntry {
    pub sandbox: Sandbox,
}

pub struct CommandEntry {
    pub sandbox_id: String,
    pub status: CommandStatus,
    pub output_log: Vec<CommandOutput>,
    pub subscribers: broadcast::Sender<CommandOutput>,
}

use rustbox_core::command::CommandStatus;

pub struct Orchestrator {
    backend: Arc<dyn VmBackend>,
    sandboxes: DashMap<String, SandboxEntry>,
    commands: Arc<DashMap<String, CommandEntry>>,
    snapshot_store: SnapshotStore,
}

impl Orchestrator {
    pub fn new(backend: Arc<dyn VmBackend>, snapshot_store: SnapshotStore) -> Self {
        Self {
            backend,
            sandboxes: DashMap::new(),
            commands: Arc::new(DashMap::new()),
            snapshot_store,
        }
    }

    pub async fn create_sandbox(&self, config: SandboxConfig) -> Result<Sandbox> {
        let id = SandboxId::new();
        self.backend.create(&id, &config).await?;
        self.backend.start(&id).await?;

        let sandbox = Sandbox {
            id: id.clone(),
            config,
            status: SandboxStatus::Running,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            stopped_at: None,
        };

        self.sandboxes.insert(
            id.to_string(),
            SandboxEntry {
                sandbox: sandbox.clone(),
            },
        );

        Ok(sandbox)
    }

    pub async fn get_sandbox(&self, id: &str) -> Result<Sandbox> {
        self.sandboxes
            .get(id)
            .map(|e| e.sandbox.clone())
            .ok_or_else(|| RustboxError::SandboxNotFound(id.to_string()))
    }

    pub async fn list_sandboxes(&self) -> Vec<Sandbox> {
        self.sandboxes.iter().map(|e| e.sandbox.clone()).collect()
    }

    pub async fn delete_sandbox(&self, id: &str) -> Result<()> {
        let sandbox_id = SandboxId(id.to_string());
        self.backend.stop(&sandbox_id, false).await?;
        self.sandboxes
            .remove(id)
            .ok_or_else(|| RustboxError::SandboxNotFound(id.to_string()))?;
        Ok(())
    }

    pub async fn update_timeout(&self, id: &str, timeout: Duration) -> Result<Sandbox> {
        let mut entry = self
            .sandboxes
            .get_mut(id)
            .ok_or_else(|| RustboxError::SandboxNotFound(id.to_string()))?;
        entry.sandbox.config.timeout = timeout;
        Ok(entry.sandbox.clone())
    }

    pub async fn update_network_policy(
        &self,
        id: &str,
        policy: NetworkPolicy,
    ) -> Result<Sandbox> {
        let mut entry = self
            .sandboxes
            .get_mut(id)
            .ok_or_else(|| RustboxError::SandboxNotFound(id.to_string()))?;
        entry.sandbox.config.network_policy = policy;
        Ok(entry.sandbox.clone())
    }

    pub async fn exec_command(&self, sandbox_id: &str, cmd: CommandRequest) -> Result<String> {
        // Verify sandbox exists and is running
        let entry = self
            .sandboxes
            .get(sandbox_id)
            .ok_or_else(|| RustboxError::SandboxNotFound(sandbox_id.to_string()))?;
        if entry.sandbox.status != SandboxStatus::Running {
            return Err(RustboxError::SandboxNotRunning(sandbox_id.to_string()));
        }
        drop(entry);

        let sid = SandboxId(sandbox_id.to_string());
        let (cmd_id, mut rx) = self.backend.exec(&sid, &cmd).await?;
        let cmd_id_str = cmd_id.to_string();

        let (broadcast_tx, _) = broadcast::channel(64);

        self.commands.insert(
            cmd_id_str.clone(),
            CommandEntry {
                sandbox_id: sandbox_id.to_string(),
                status: CommandStatus::Running,
                output_log: Vec::new(),
                subscribers: broadcast_tx.clone(),
            },
        );

        // Spawn a task to collect output from the backend
        let commands = Arc::clone(&self.commands);
        let cid = cmd_id_str.clone();
        tokio::spawn(async move {
            while let Some(output) = rx.recv().await {
                let is_exit = matches!(&output, CommandOutput::Exit(_));
                // Store in log and broadcast
                if let Some(mut entry) = commands.get_mut(&cid) {
                    entry.output_log.push(output.clone());
                    let _ = entry.subscribers.send(output);
                    if is_exit {
                        if let Some(CommandOutput::Exit(code)) = entry.output_log.last() {
                            entry.status = CommandStatus::Completed(*code);
                        }
                        break;
                    }
                }
            }
        });

        Ok(cmd_id_str)
    }

    pub async fn get_command(
        &self,
        cmd_id: &str,
    ) -> Result<(String, CommandStatus, Vec<CommandOutput>)> {
        let entry = self
            .commands
            .get(cmd_id)
            .ok_or_else(|| RustboxError::CommandNotFound(cmd_id.to_string()))?;
        Ok((
            entry.sandbox_id.clone(),
            entry.status.clone(),
            entry.output_log.clone(),
        ))
    }

    pub fn subscribe_command_logs(
        &self,
        cmd_id: &str,
    ) -> Result<broadcast::Receiver<CommandOutput>> {
        let entry = self
            .commands
            .get(cmd_id)
            .ok_or_else(|| RustboxError::CommandNotFound(cmd_id.to_string()))?;
        Ok(entry.subscribers.subscribe())
    }

    pub async fn kill_command(&self, sandbox_id: &str, cmd_id: &str, signal: i32) -> Result<()> {
        let sid = SandboxId(sandbox_id.to_string());
        let cid = CommandId(cmd_id.to_string());
        self.backend.kill_command(&sid, &cid, signal).await
    }

    pub async fn write_file(&self, sandbox_id: &str, path: &str, content: &[u8]) -> Result<()> {
        let sid = SandboxId(sandbox_id.to_string());
        self.backend.write_file(&sid, path, content).await
    }

    pub async fn read_file(&self, sandbox_id: &str, path: &str) -> Result<Vec<u8>> {
        let sid = SandboxId(sandbox_id.to_string());
        self.backend.read_file(&sid, path).await
    }

    pub async fn mkdir(&self, sandbox_id: &str, path: &str) -> Result<()> {
        let sid = SandboxId(sandbox_id.to_string());
        self.backend.mkdir(&sid, path).await
    }

    pub async fn create_snapshot(
        &self,
        sandbox_id: &str,
        description: Option<String>,
    ) -> Result<SnapshotMetadata> {
        let sid = SandboxId(sandbox_id.to_string());
        let snap_id = self.backend.snapshot_create(&sid).await?;

        let metadata = SnapshotMetadata {
            id: snap_id.to_string(),
            sandbox_id: sandbox_id.to_string(),
            created_at: Utc::now(),
            expires_at: None,
            size_bytes: 0,
            description,
        };
        self.snapshot_store
            .save(&metadata)
            .map_err(|e| RustboxError::Storage(e.to_string()))?;

        Ok(metadata)
    }

    pub async fn get_snapshot(&self, id: &str) -> Result<SnapshotMetadata> {
        self.snapshot_store
            .get(id)
            .map_err(|e| RustboxError::Storage(e.to_string()))?
            .ok_or_else(|| RustboxError::SnapshotNotFound(id.to_string()))
    }

    pub async fn delete_snapshot(&self, id: &str) -> Result<()> {
        let deleted = self
            .snapshot_store
            .delete(id)
            .map_err(|e| RustboxError::Storage(e.to_string()))?;
        if !deleted {
            return Err(RustboxError::SnapshotNotFound(id.to_string()));
        }
        Ok(())
    }

    pub async fn get_metrics(&self, sandbox_id: &str) -> Result<SandboxMetrics> {
        let sid = SandboxId(sandbox_id.to_string());
        self.backend.metrics(&sid).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustbox_core::sandbox::{CpuCount, Runtime};
    use rustbox_vm::mock_backend::MockBackend;
    use std::collections::HashMap;

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

    fn make_test_orchestrator() -> Orchestrator {
        let backend = Arc::new(MockBackend::new());
        let snapshot_store = SnapshotStore::new_in_memory().unwrap();
        Orchestrator::new(backend, snapshot_store)
    }

    #[tokio::test]
    async fn create_and_get_sandbox() {
        let orch = make_test_orchestrator();
        let sandbox = orch.create_sandbox(test_config()).await.unwrap();
        assert_eq!(sandbox.status, SandboxStatus::Running);

        let fetched = orch.get_sandbox(&sandbox.id.to_string()).await.unwrap();
        assert_eq!(fetched.id, sandbox.id);
        assert_eq!(fetched.status, SandboxStatus::Running);
    }

    #[tokio::test]
    async fn list_sandboxes() {
        let orch = make_test_orchestrator();
        orch.create_sandbox(test_config()).await.unwrap();
        orch.create_sandbox(test_config()).await.unwrap();

        let list = orch.list_sandboxes().await;
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn delete_sandbox() {
        let orch = make_test_orchestrator();
        let sandbox = orch.create_sandbox(test_config()).await.unwrap();
        let id = sandbox.id.to_string();

        orch.delete_sandbox(&id).await.unwrap();
        let result = orch.get_sandbox(&id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn delete_nonexistent() {
        let orch = make_test_orchestrator();
        let result = orch.delete_sandbox("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn update_timeout() {
        let orch = make_test_orchestrator();
        let sandbox = orch.create_sandbox(test_config()).await.unwrap();
        let id = sandbox.id.to_string();

        let updated = orch
            .update_timeout(&id, Duration::from_secs(600))
            .await
            .unwrap();
        assert_eq!(updated.config.timeout, Duration::from_secs(600));
    }

    #[tokio::test]
    async fn update_network_policy() {
        let orch = make_test_orchestrator();
        let sandbox = orch.create_sandbox(test_config()).await.unwrap();
        let id = sandbox.id.to_string();

        let policy = NetworkPolicy {
            mode: rustbox_core::network::NetworkMode::DenyAll,
            ..NetworkPolicy::default()
        };
        let updated = orch.update_network_policy(&id, policy).await.unwrap();
        match updated.config.network_policy.mode {
            rustbox_core::network::NetworkMode::DenyAll => {}
            _ => panic!("expected DenyAll"),
        }
    }

    #[tokio::test]
    async fn exec_command_returns_id() {
        let orch = make_test_orchestrator();
        let sandbox = orch.create_sandbox(test_config()).await.unwrap();
        let id = sandbox.id.to_string();

        let cmd_id = orch.exec_command(&id, test_cmd()).await.unwrap();
        assert!(!cmd_id.is_empty());
    }

    #[tokio::test]
    async fn exec_on_nonexistent() {
        let orch = make_test_orchestrator();
        let result = orch.exec_command("nonexistent", test_cmd()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn write_and_read_file() {
        let orch = make_test_orchestrator();
        let sandbox = orch.create_sandbox(test_config()).await.unwrap();
        let id = sandbox.id.to_string();

        orch.write_file(&id, "/tmp/test.txt", b"hello")
            .await
            .unwrap();
        let content = orch.read_file(&id, "/tmp/test.txt").await.unwrap();
        // MockBackend always returns "mock file content"
        assert_eq!(content, b"mock file content");
    }

    #[tokio::test]
    async fn mkdir_succeeds() {
        let orch = make_test_orchestrator();
        let sandbox = orch.create_sandbox(test_config()).await.unwrap();
        let id = sandbox.id.to_string();

        orch.mkdir(&id, "/tmp/newdir").await.unwrap();
    }

    #[tokio::test]
    async fn create_and_get_snapshot() {
        let orch = make_test_orchestrator();
        let sandbox = orch.create_sandbox(test_config()).await.unwrap();
        let id = sandbox.id.to_string();

        let meta = orch
            .create_snapshot(&id, Some("test snapshot".to_string()))
            .await
            .unwrap();
        assert!(!meta.id.is_empty());
        assert_eq!(meta.sandbox_id, id);
        assert_eq!(meta.description, Some("test snapshot".to_string()));

        let fetched = orch.get_snapshot(&meta.id).await.unwrap();
        assert_eq!(fetched.id, meta.id);
    }

    #[tokio::test]
    async fn delete_snapshot() {
        let orch = make_test_orchestrator();
        let sandbox = orch.create_sandbox(test_config()).await.unwrap();
        let id = sandbox.id.to_string();

        let meta = orch.create_snapshot(&id, None).await.unwrap();
        orch.delete_snapshot(&meta.id).await.unwrap();

        let result = orch.get_snapshot(&meta.id).await;
        assert!(result.is_err());
    }
}
