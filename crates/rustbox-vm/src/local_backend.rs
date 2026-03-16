use async_trait::async_trait;
use dashmap::DashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex};

use rustbox_core::{
    backend::VmBackend, CommandId, CommandOutput, CommandRequest, Result, RustboxError, SandboxConfig,
    SandboxId, SandboxMetrics, SandboxStatus, SnapshotId,
};

const SANDBOX_BASE_DIR: &str = "/tmp/rustbox-sandboxes";

struct LocalSandbox {
    #[allow(dead_code)]
    config: SandboxConfig,
    status: SandboxStatus,
    work_dir: PathBuf,
}

/// A backend that executes commands locally using `tokio::process::Command`
/// and does real file I/O in per-sandbox temp directories. No VM isolation —
/// intended for macOS development.
pub struct LocalBackend {
    sandboxes: DashMap<String, LocalSandbox>,
    /// Track running child processes for kill support.
    running_commands: DashMap<String, Arc<Mutex<Child>>>,
}

impl LocalBackend {
    pub fn new() -> Self {
        Self {
            sandboxes: DashMap::new(),
            running_commands: DashMap::new(),
        }
    }

    fn resolve_path(&self, sandbox_id: &str, path: &str) -> Result<PathBuf> {
        let sandbox = self
            .sandboxes
            .get(sandbox_id)
            .ok_or_else(|| RustboxError::SandboxNotFound(sandbox_id.to_string()))?;
        let clean = path.strip_prefix('/').unwrap_or(path);
        Ok(sandbox.work_dir.join(clean))
    }
}

impl Default for LocalBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VmBackend for LocalBackend {
    async fn create(&self, id: &SandboxId, config: &SandboxConfig) -> Result<()> {
        let key = id.to_string();
        let work_dir = PathBuf::from(SANDBOX_BASE_DIR).join(&key);
        tokio::fs::create_dir_all(&work_dir)
            .await
            .map_err(|e| RustboxError::VmBackend(format!("failed to create work dir: {e}")))?;
        self.sandboxes.insert(
            key,
            LocalSandbox {
                config: config.clone(),
                status: SandboxStatus::Pending,
                work_dir,
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
            .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;

        // Kill all tracked commands for this sandbox.
        let prefix = format!("{key}:");
        let to_remove: Vec<String> = self
            .running_commands
            .iter()
            .filter(|entry| entry.key().starts_with(&prefix))
            .map(|entry| entry.key().clone())
            .collect();
        for cmd_key in to_remove {
            if let Some((_, child)) = self.running_commands.remove(&cmd_key) {
                let _ = child.lock().await.kill().await;
            }
        }

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
        cmd: &CommandRequest,
    ) -> Result<(CommandId, mpsc::Receiver<CommandOutput>)> {
        let key = id.to_string();
        let sandbox = self
            .sandboxes
            .get(&key)
            .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;

        let effective_cwd = match &cmd.cwd {
            Some(cwd) => {
                let clean = cwd.strip_prefix('/').unwrap_or(cwd);
                sandbox.work_dir.join(clean)
            }
            None => sandbox.work_dir.clone(),
        };
        drop(sandbox);

        let mut command = Command::new(&cmd.cmd);
        command.args(&cmd.args);
        command.current_dir(&effective_cwd);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        if let Some(env) = &cmd.env {
            command.envs(env.iter());
        }

        let mut child = command
            .spawn()
            .map_err(|e| RustboxError::VmBackend(format!("failed to spawn process: {e}")))?;

        let cmd_id = CommandId::new();
        let (tx, rx) = mpsc::channel(64);

        let child_stdout = child.stdout.take();
        let child_stderr = child.stderr.take();

        let child = Arc::new(Mutex::new(child));
        let cmd_tracking_key = format!("{key}:{cmd_id}");
        self.running_commands
            .insert(cmd_tracking_key.clone(), child.clone());

        let running_commands = self.running_commands.clone();
        let tracking_key = cmd_tracking_key.clone();

        tokio::spawn(async move {
            // Read stdout in a separate task.
            let tx_stdout = tx.clone();
            let stdout_handle = tokio::spawn(async move {
                if let Some(mut stdout) = child_stdout {
                    let mut buf = [0u8; 4096];
                    loop {
                        match stdout.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                if tx_stdout
                                    .send(CommandOutput::Stdout(buf[..n].to_vec()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
            });

            // Read stderr in a separate task.
            let tx_stderr = tx.clone();
            let stderr_handle = tokio::spawn(async move {
                if let Some(mut stderr) = child_stderr {
                    let mut buf = [0u8; 4096];
                    loop {
                        match stderr.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                if tx_stderr
                                    .send(CommandOutput::Stderr(buf[..n].to_vec()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
            });

            // Wait for both readers to finish.
            let _ = stdout_handle.await;
            let _ = stderr_handle.await;

            // Wait for the process to exit.
            let exit_code = {
                let mut child = child.lock().await;
                match child.wait().await {
                    Ok(status) => status.code().unwrap_or(-1),
                    Err(_) => -1,
                }
            };

            let _ = tx.send(CommandOutput::Exit(exit_code)).await;
            running_commands.remove(&tracking_key);
        });

        Ok((cmd_id, rx))
    }

    async fn kill_command(
        &self,
        id: &SandboxId,
        cmd_id: &CommandId,
        _signal: i32,
    ) -> Result<()> {
        let key = id.to_string();
        if !self.sandboxes.contains_key(&key) {
            return Err(RustboxError::SandboxNotFound(key));
        }

        let cmd_key = format!("{key}:{cmd_id}");
        if let Some((_, child)) = self.running_commands.remove(&cmd_key) {
            child
                .lock()
                .await
                .kill()
                .await
                .map_err(|e| RustboxError::VmBackend(format!("failed to kill process: {e}")))?;
        }
        Ok(())
    }

    async fn write_file(
        &self,
        id: &SandboxId,
        path: &str,
        content: &[u8],
    ) -> Result<()> {
        let full_path = self.resolve_path(&id.to_string(), path)?;
        if let Some(parent) = full_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| RustboxError::VmBackend(format!("failed to create parent dirs: {e}")))?;
        }
        tokio::fs::write(&full_path, content)
            .await
            .map_err(|e| RustboxError::VmBackend(format!("failed to write file: {e}")))?;
        Ok(())
    }

    async fn read_file(&self, id: &SandboxId, path: &str) -> Result<Vec<u8>> {
        let full_path = self.resolve_path(&id.to_string(), path)?;
        tokio::fs::read(&full_path)
            .await
            .map_err(|e| RustboxError::VmBackend(format!("failed to read file: {e}")))
    }

    async fn mkdir(&self, id: &SandboxId, path: &str) -> Result<()> {
        let full_path = self.resolve_path(&id.to_string(), path)?;
        tokio::fs::create_dir_all(&full_path)
            .await
            .map_err(|e| RustboxError::VmBackend(format!("failed to create directory: {e}")))?;
        Ok(())
    }

    async fn snapshot_create(&self, id: &SandboxId) -> Result<SnapshotId> {
        let key = id.to_string();
        if !self.sandboxes.contains_key(&key) {
            return Err(RustboxError::SandboxNotFound(key));
        }
        // Real snapshot support would archive the work_dir; for now return an ID.
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
    use rustbox_core::command::CommandRequest;
    use rustbox_core::network::NetworkPolicy;
    use rustbox_core::sandbox::{CpuCount, Runtime, SandboxConfig};
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

    #[tokio::test]
    async fn create_makes_work_dir() {
        let backend = LocalBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        let work_dir = PathBuf::from(SANDBOX_BASE_DIR).join(id.to_string());
        assert!(work_dir.exists());
        // Cleanup.
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn exec_echo_hello() {
        let backend = LocalBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        backend.start(&id).await.unwrap();

        let cmd = CommandRequest {
            cmd: "echo".into(),
            args: vec!["hello".into()],
            cwd: None,
            env: None,
            sudo: false,
            detached: false,
        };

        let (_cmd_id, mut rx) = backend.exec(&id, &cmd).await.unwrap();

        let mut stdout_data = Vec::new();
        let mut exit_code = None;
        while let Some(output) = rx.recv().await {
            match output {
                CommandOutput::Stdout(data) => stdout_data.extend_from_slice(&data),
                CommandOutput::Exit(code) => {
                    exit_code = Some(code);
                    break;
                }
                _ => {}
            }
        }

        assert_eq!(String::from_utf8_lossy(&stdout_data).trim(), "hello");
        assert_eq!(exit_code, Some(0));

        // Cleanup.
        let work_dir = PathBuf::from(SANDBOX_BASE_DIR).join(id.to_string());
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn exec_exit_code() {
        let backend = LocalBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        backend.start(&id).await.unwrap();

        let cmd = CommandRequest {
            cmd: "false".into(),
            args: vec![],
            cwd: None,
            env: None,
            sudo: false,
            detached: false,
        };

        let (_cmd_id, mut rx) = backend.exec(&id, &cmd).await.unwrap();

        let mut exit_code = None;
        while let Some(output) = rx.recv().await {
            if let CommandOutput::Exit(code) = output {
                exit_code = Some(code);
                break;
            }
        }

        assert_eq!(exit_code, Some(1));

        let work_dir = PathBuf::from(SANDBOX_BASE_DIR).join(id.to_string());
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn exec_with_env() {
        let backend = LocalBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        backend.start(&id).await.unwrap();

        let mut env = HashMap::new();
        env.insert("RUSTBOX_TEST_VAR".to_string(), "test_value_42".to_string());

        let cmd = CommandRequest {
            cmd: "env".into(),
            args: vec![],
            cwd: None,
            env: Some(env),
            sudo: false,
            detached: false,
        };

        let (_cmd_id, mut rx) = backend.exec(&id, &cmd).await.unwrap();

        let mut stdout_data = Vec::new();
        while let Some(output) = rx.recv().await {
            match output {
                CommandOutput::Stdout(data) => stdout_data.extend_from_slice(&data),
                CommandOutput::Exit(_) => break,
                _ => {}
            }
        }

        let output = String::from_utf8_lossy(&stdout_data);
        assert!(
            output.contains("RUSTBOX_TEST_VAR=test_value_42"),
            "expected env var in output, got: {output}"
        );

        let work_dir = PathBuf::from(SANDBOX_BASE_DIR).join(id.to_string());
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn write_and_read_file() {
        let backend = LocalBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();

        let content = b"hello from rustbox";
        backend
            .write_file(&id, "subdir/test.txt", content)
            .await
            .unwrap();
        let read_back = backend.read_file(&id, "subdir/test.txt").await.unwrap();
        assert_eq!(read_back, content);

        let work_dir = PathBuf::from(SANDBOX_BASE_DIR).join(id.to_string());
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn mkdir_creates_dir() {
        let backend = LocalBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();

        backend.mkdir(&id, "a/b/c").await.unwrap();

        let dir = PathBuf::from(SANDBOX_BASE_DIR)
            .join(id.to_string())
            .join("a/b/c");
        assert!(dir.is_dir());

        let work_dir = PathBuf::from(SANDBOX_BASE_DIR).join(id.to_string());
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }

    #[tokio::test]
    async fn stop_cleans_status() {
        let backend = LocalBackend::new();
        let id = SandboxId::new();
        backend.create(&id, &test_config()).await.unwrap();
        backend.start(&id).await.unwrap();
        assert_eq!(backend.status(&id).await.unwrap(), SandboxStatus::Running);
        backend.stop(&id, false).await.unwrap();
        assert_eq!(backend.status(&id).await.unwrap(), SandboxStatus::Stopped);

        let work_dir = PathBuf::from(SANDBOX_BASE_DIR).join(id.to_string());
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
    }
}
