use std::path::PathBuf;

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::sync::mpsc;
use tracing::{info, warn};

use rustbox_core::{
    CommandId, CommandOutput, CommandRequest, Result, RustboxError,
    SandboxConfig, SandboxId, SandboxMetrics, SandboxStatus, SnapshotId,
    backend::VmBackend,
    protocol::AgentRequest,
};

use super::api::FirecrackerClient;
use super::process::{FirecrackerProcess, FirecrackerProcessConfig};
use crate::agent_client::AgentClient;

/// Configuration for the Firecracker backend.
#[derive(Clone, Debug)]
pub struct FirecrackerBackendConfig {
    /// Path to the firecracker binary.
    pub firecracker_bin: PathBuf,
    /// Path to the kernel image.
    pub kernel_path: PathBuf,
    /// Directory containing rootfs images.
    pub rootfs_dir: PathBuf,
    /// Directory to store per-VM state (sockets, logs, snapshots).
    pub state_dir: PathBuf,
    /// Base directory for vsock UDS paths.
    pub vsock_base_dir: PathBuf,
}

/// Per-sandbox VM instance state.
struct VmInstance {
    config: SandboxConfig,
    status: SandboxStatus,
    process: Option<FirecrackerProcess>,
    socket_path: PathBuf,
    vsock_path: PathBuf,
    /// Guest CID assigned to this VM.
    guest_cid: u32,
}

/// The Firecracker `VmBackend` implementation.
pub struct FirecrackerBackend {
    config: FirecrackerBackendConfig,
    instances: DashMap<String, VmInstance>,
    /// Monotonically increasing CID counter (starting at 3, since 0-2 are reserved).
    next_cid: std::sync::atomic::AtomicU32,
}

impl FirecrackerBackend {
    pub fn new(config: FirecrackerBackendConfig) -> Self {
        Self {
            config,
            instances: DashMap::new(),
            next_cid: std::sync::atomic::AtomicU32::new(3),
        }
    }

    fn allocate_cid(&self) -> u32 {
        self.next_cid
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    fn socket_path_for(&self, id: &SandboxId) -> PathBuf {
        self.config.state_dir.join(format!("{}.sock", id))
    }

    fn vsock_path_for(&self, id: &SandboxId) -> PathBuf {
        self.config.vsock_base_dir.join(format!("{}.vsock", id))
    }

    fn log_path_for(&self, id: &SandboxId) -> PathBuf {
        self.config.state_dir.join(format!("{}.log", id))
    }

    /// Build the rootfs path for the given sandbox config.
    fn rootfs_path_for(&self, config: &SandboxConfig) -> PathBuf {
        let runtime_name = format!("{:?}", config.runtime).to_lowercase();
        self.config.rootfs_dir.join(format!("{runtime_name}.ext4"))
    }

    /// Connect to the guest agent for a given sandbox.
    fn agent_client_for(&self, instance: &VmInstance) -> AgentClient {
        // For now, use TCP on port 5123 as fallback until vsock is wired up.
        AgentClient::new_tcp("127.0.0.1".to_string(), 5123 + instance.guest_cid as u16)
    }
}

#[async_trait]
impl VmBackend for FirecrackerBackend {
    async fn create(&self, id: &SandboxId, config: &SandboxConfig) -> Result<()> {
        let cid = self.allocate_cid();
        let socket_path = self.socket_path_for(id);
        let vsock_path = self.vsock_path_for(id);

        info!(%id, cid = cid, "creating sandbox");

        let instance = VmInstance {
            config: config.clone(),
            status: SandboxStatus::Pending,
            process: None,
            socket_path,
            vsock_path,
            guest_cid: cid,
        };

        self.instances.insert(id.to_string(), instance);
        Ok(())
    }

    async fn start(&self, id: &SandboxId) -> Result<()> {
        let key = id.to_string();
        let (socket_path, vsock_path, guest_cid, sandbox_config) = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            (
                inst.socket_path.clone(),
                inst.vsock_path.clone(),
                inst.guest_cid,
                inst.config.clone(),
            )
        };

        info!(%id, "starting sandbox");

        // Ensure state directory exists.
        std::fs::create_dir_all(&self.config.state_dir).map_err(|e| {
            RustboxError::VmBackend(format!("create state dir: {e}"))
        })?;
        std::fs::create_dir_all(&self.config.vsock_base_dir).map_err(|e| {
            RustboxError::VmBackend(format!("create vsock dir: {e}"))
        })?;

        // Spawn Firecracker process.
        let proc_config = FirecrackerProcessConfig {
            firecracker_bin: self.config.firecracker_bin.clone(),
            log_path: self.log_path_for(id),
        };
        let process = FirecrackerProcess::spawn(&socket_path, &proc_config).await?;

        // Configure the VM via the Firecracker API.
        let client = FirecrackerClient::new(&socket_path);

        let rootfs_path = self.rootfs_path_for(&sandbox_config);
        let vcpu_count = sandbox_config.cpu_count as u8;

        // Memory in MiB based on CPU count (256 MiB per vCPU, minimum 512).
        let mem_size_mib = std::cmp::max(512, vcpu_count as u32 * 256);

        let boot_args = "console=ttyS0 reboot=k panic=1 pci=off";

        client
            .put_boot_source(
                self.config.kernel_path.to_str().unwrap_or("vmlinux"),
                boot_args,
            )
            .await?;

        client
            .put_drive(
                "rootfs",
                rootfs_path.to_str().unwrap_or("rootfs.ext4"),
                true,
                false,
            )
            .await?;

        client.put_machine_config(vcpu_count, mem_size_mib).await?;

        client
            .put_vsock(
                vsock_path.to_str().unwrap_or("vsock.sock"),
                guest_cid,
            )
            .await?;

        client.start_instance().await?;

        // Update instance state.
        if let Some(mut inst) = self.instances.get_mut(&key) {
            inst.process = Some(process);
            inst.status = SandboxStatus::Running;
        }

        info!(%id, "sandbox started");
        Ok(())
    }

    async fn stop(&self, id: &SandboxId, blocking: bool) -> Result<()> {
        let key = id.to_string();
        info!(%id, blocking = blocking, "stopping sandbox");

        // Try graceful stop first.
        {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;

            if inst.status != SandboxStatus::Running {
                return Err(RustboxError::SandboxNotRunning(key.clone()));
            }

            let client = FirecrackerClient::new(&inst.socket_path);
            if let Err(e) = client.stop_instance().await {
                warn!(%id, error = %e, "graceful stop failed, will force kill");
            }
        }

        // Kill the process.
        if let Some(mut inst) = self.instances.get_mut(&key) {
            inst.status = SandboxStatus::Stopping;
            if let Some(ref mut proc) = inst.process {
                proc.kill().await?;
            }
            inst.process = None;
            inst.status = SandboxStatus::Stopped;
        }

        info!(%id, "sandbox stopped");
        Ok(())
    }

    async fn status(&self, id: &SandboxId) -> Result<SandboxStatus> {
        let key = id.to_string();
        let inst = self
            .instances
            .get(&key)
            .ok_or_else(|| RustboxError::SandboxNotFound(key))?;
        Ok(inst.status.clone())
    }

    async fn exec(
        &self,
        id: &SandboxId,
        cmd: &CommandRequest,
    ) -> Result<(CommandId, mpsc::Receiver<CommandOutput>)> {
        let key = id.to_string();
        let agent = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            if inst.status != SandboxStatus::Running {
                return Err(RustboxError::SandboxNotRunning(key));
            }
            self.agent_client_for(&inst)
        };

        let cmd_id = CommandId::new();
        let (tx, rx) = mpsc::channel(64);

        let request = AgentRequest::Exec(cmd.clone());
        let cmd_id_clone = cmd_id.clone();

        tokio::spawn(async move {
            match agent.exec_streaming(request, cmd_id_clone, tx).await {
                Ok(()) => {}
                Err(e) => {
                    tracing::error!(error = %e, "agent exec streaming failed");
                }
            }
        });

        Ok((cmd_id, rx))
    }

    async fn kill_command(
        &self,
        id: &SandboxId,
        cmd_id: &CommandId,
        signal: i32,
    ) -> Result<()> {
        let key = id.to_string();
        let agent = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            if inst.status != SandboxStatus::Running {
                return Err(RustboxError::SandboxNotRunning(key));
            }
            self.agent_client_for(&inst)
        };

        let request = AgentRequest::Kill {
            command_id: cmd_id.to_string(),
            signal,
        };
        let mut conn = agent.connect().await?;
        conn.send_request(&request).await?;
        let _resp = conn.recv_response().await?;
        Ok(())
    }

    async fn write_file(
        &self,
        id: &SandboxId,
        path: &str,
        content: &[u8],
    ) -> Result<()> {
        let key = id.to_string();
        let agent = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            if inst.status != SandboxStatus::Running {
                return Err(RustboxError::SandboxNotRunning(key));
            }
            self.agent_client_for(&inst)
        };

        let request = AgentRequest::WriteFile {
            path: path.to_string(),
            content: content.to_vec(),
        };
        let mut conn = agent.connect().await?;
        conn.send_request(&request).await?;
        let _resp = conn.recv_response().await?;
        Ok(())
    }

    async fn read_file(&self, id: &SandboxId, path: &str) -> Result<Vec<u8>> {
        let key = id.to_string();
        let agent = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            if inst.status != SandboxStatus::Running {
                return Err(RustboxError::SandboxNotRunning(key));
            }
            self.agent_client_for(&inst)
        };

        let request = AgentRequest::ReadFile {
            path: path.to_string(),
        };
        let mut conn = agent.connect().await?;
        conn.send_request(&request).await?;
        let resp = conn.recv_response().await?;

        match resp {
            rustbox_core::protocol::AgentResponse::FileContent { data } => Ok(data),
            rustbox_core::protocol::AgentResponse::Error { message } => {
                Err(RustboxError::AgentComm(message))
            }
            other => Err(RustboxError::AgentComm(format!(
                "unexpected response: {other:?}"
            ))),
        }
    }

    async fn mkdir(&self, id: &SandboxId, path: &str) -> Result<()> {
        let key = id.to_string();
        let agent = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            if inst.status != SandboxStatus::Running {
                return Err(RustboxError::SandboxNotRunning(key));
            }
            self.agent_client_for(&inst)
        };

        let request = AgentRequest::Mkdir {
            path: path.to_string(),
        };
        let mut conn = agent.connect().await?;
        conn.send_request(&request).await?;
        let _resp = conn.recv_response().await?;
        Ok(())
    }

    async fn snapshot_create(&self, id: &SandboxId) -> Result<SnapshotId> {
        let key = id.to_string();
        let socket_path = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            if inst.status != SandboxStatus::Running {
                return Err(RustboxError::SandboxNotRunning(key));
            }
            inst.socket_path.clone()
        };

        let snap_id = SnapshotId::new();
        let snap_dir = self
            .config
            .state_dir
            .join("snapshots")
            .join(id.to_string());
        std::fs::create_dir_all(&snap_dir)
            .map_err(|e| RustboxError::VmBackend(format!("create snapshot dir: {e}")))?;

        let snapshot_path = snap_dir.join(format!("{snap_id}.snap"));
        let mem_path = snap_dir.join(format!("{snap_id}.mem"));

        let client = FirecrackerClient::new(&socket_path);
        client.pause_instance().await?;
        client
            .create_snapshot(
                snapshot_path.to_str().unwrap_or("snapshot"),
                mem_path.to_str().unwrap_or("mem"),
            )
            .await?;

        Ok(snap_id)
    }

    async fn snapshot_restore(
        &self,
        id: &SandboxId,
        snap: &SnapshotId,
    ) -> Result<()> {
        let key = id.to_string();
        let socket_path = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            inst.socket_path.clone()
        };

        let snap_dir = self
            .config
            .state_dir
            .join("snapshots")
            .join(id.to_string());
        let snapshot_path = snap_dir.join(format!("{snap}.snap"));
        let mem_path = snap_dir.join(format!("{snap}.mem"));

        if !snapshot_path.exists() {
            return Err(RustboxError::SnapshotNotFound(snap.to_string()));
        }

        let client = FirecrackerClient::new(&socket_path);
        client
            .load_snapshot(
                snapshot_path.to_str().unwrap_or("snapshot"),
                mem_path.to_str().unwrap_or("mem"),
            )
            .await?;

        if let Some(mut inst) = self.instances.get_mut(&key) {
            inst.status = SandboxStatus::Running;
        }

        Ok(())
    }

    async fn metrics(&self, id: &SandboxId) -> Result<SandboxMetrics> {
        let key = id.to_string();
        let agent = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            if inst.status != SandboxStatus::Running {
                return Err(RustboxError::SandboxNotRunning(key));
            }
            self.agent_client_for(&inst)
        };

        let request = AgentRequest::Metrics;
        let mut conn = agent.connect().await?;
        conn.send_request(&request).await?;
        let resp = conn.recv_response().await?;

        match resp {
            rustbox_core::protocol::AgentResponse::MetricsResult(m) => Ok(m),
            rustbox_core::protocol::AgentResponse::Error { message } => {
                Err(RustboxError::AgentComm(message))
            }
            other => Err(RustboxError::AgentComm(format!(
                "unexpected response: {other:?}"
            ))),
        }
    }
}
