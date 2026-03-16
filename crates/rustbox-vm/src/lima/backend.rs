use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use dashmap::DashMap;
use tokio::process::Child;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use rustbox_core::{
    backend::VmBackend, CommandId, CommandOutput, CommandRequest, Result, RustboxError,
    SandboxConfig, SandboxId, SandboxMetrics, SandboxStatus, SnapshotId,
    protocol::AgentRequest,
};

use super::manager::{LimaConfig, LimaManager};
use crate::agent_client::AgentClient;
use crate::firecracker::api::FirecrackerClient;

/// Agent port inside the microVM guest.
const AGENT_GUEST_PORT: u16 = 5123;

/// Per-sandbox VM instance state managed by the Lima+Firecracker backend.
struct LimaVmInstance {
    config: SandboxConfig,
    status: SandboxStatus,
    /// Path to the Firecracker API socket inside Lima (e.g., /tmp/fc-{id}.sock).
    remote_socket_path: String,
    /// Firecracker process PID inside Lima.
    remote_pid: Option<u32>,
    /// SSH port-forward process for the Firecracker API socket.
    api_forward: Option<Child>,
    /// Local TCP port mapped to the Firecracker API socket.
    api_local_port: u16,
    /// SSH port-forward process for the guest agent.
    agent_forward: Option<Child>,
    /// Local TCP port mapped to the guest agent.
    agent_local_port: u16,
    /// Guest CID (used for TAP device naming and IP addressing).
    guest_cid: u32,
    /// TAP device name inside Lima (e.g., tap-{cid}).
    tap_name: String,
}

/// VmBackend implementation that runs Firecracker microVMs inside a Lima Linux VM.
///
/// Architecture: macOS -> Lima (Apple Virtualization.framework) -> Firecracker (microVMs)
///
/// The Lima VM provides the Linux+KVM environment needed by Firecracker.
/// SSH port-forwarding tunnels the Firecracker API socket and guest agent
/// connections back to the macOS host.
pub struct LimaFirecrackerBackend {
    manager: LimaManager,
    instances: DashMap<String, LimaVmInstance>,
    next_cid: AtomicU32,
    /// Next available local port for SSH forwarding.
    next_port: AtomicU32,
}

impl LimaFirecrackerBackend {
    pub async fn new(config: LimaConfig) -> Result<Self> {
        let manager = LimaManager::new(config);
        Ok(Self {
            manager,
            instances: DashMap::new(),
            next_cid: AtomicU32::new(3), // CIDs 0-2 are reserved.
            next_port: AtomicU32::new(10000),
        })
    }

    fn allocate_cid(&self) -> u32 {
        self.next_cid.fetch_add(1, Ordering::Relaxed)
    }

    /// Allocate an ephemeral TCP port by binding to port 0 and reading the
    /// OS-assigned port. This avoids collisions with stale SSH tunnel processes
    /// left over from previous daemon runs.
    fn allocate_port(&self) -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .unwrap_or_else(|_| self.next_port.fetch_add(1, Ordering::Relaxed) as u16)
    }

    fn agent_client_for(&self, instance: &LimaVmInstance) -> AgentClient {
        AgentClient::new_tcp("127.0.0.1".to_string(), instance.agent_local_port)
    }

    /// Get the guest IP address for a given CID.
    /// Addressing scheme: 172.16.{cid}.2/30 (host side is .1).
    fn guest_ip(cid: u32) -> String {
        format!("172.16.{cid}.2")
    }
}

#[async_trait]
impl VmBackend for LimaFirecrackerBackend {
    async fn create(&self, id: &SandboxId, config: &SandboxConfig) -> Result<()> {
        // Ensure Lima VM is ready (idempotent).
        self.manager.ensure_ready().await?;

        let cid = self.allocate_cid();
        let api_port = self.allocate_port();
        let agent_port = self.allocate_port();
        let tap_name = format!("tap-{cid}");
        let remote_socket = format!("/tmp/fc-{id}.sock");

        info!(%id, cid = cid, "creating lima sandbox");

        let instance = LimaVmInstance {
            config: config.clone(),
            status: SandboxStatus::Pending,
            remote_socket_path: remote_socket,
            remote_pid: None,
            api_forward: None,
            api_local_port: api_port,
            agent_forward: None,
            agent_local_port: agent_port,
            guest_cid: cid,
            tap_name,
        };

        self.instances.insert(id.to_string(), instance);
        Ok(())
    }

    async fn start(&self, id: &SandboxId) -> Result<()> {
        let key = id.to_string();
        let (remote_socket, cid, tap_name, api_port, agent_port, sandbox_config) = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            (
                inst.remote_socket_path.clone(),
                inst.guest_cid,
                inst.tap_name.clone(),
                inst.api_local_port,
                inst.agent_local_port,
                inst.config.clone(),
            )
        };

        info!(%id, cid = cid, "starting lima sandbox");
        let ssh = self.manager.ssh();

        // 1. Clean up any stale socket.
        let _ = ssh.exec(&format!("rm -f {remote_socket}")).await;

        // 2. Spawn Firecracker inside Lima.
        let remote_pid = ssh
            .spawn_background(&format!("firecracker --api-sock {remote_socket}"))
            .await?;
        debug!(%id, pid = remote_pid, "firecracker spawned inside lima");

        // 3. Wait for the API socket to appear.
        let mut retries = 0;
        loop {
            let check = ssh
                .exec(&format!("test -S {remote_socket} && echo READY || echo WAITING"))
                .await?;
            if check.trim() == "READY" {
                break;
            }
            retries += 1;
            if retries > 50 {
                return Err(RustboxError::Timeout(
                    "Firecracker API socket did not appear in Lima".to_string(),
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // 4. Start SSH forward for the API socket.
        let api_forward = ssh.forward_unix_socket(api_port, &remote_socket).await?;

        // 5. Configure the VM via the Firecracker API (over TCP forward).
        let client = FirecrackerClient::new_tcp("127.0.0.1", api_port);

        let vcpu_count = sandbox_config.cpu_count as u8;
        let mem_size_mib = std::cmp::max(512, vcpu_count as u32 * 256);
        let guest_ip = Self::guest_ip(cid);
        let host_ip = format!("172.16.{cid}.1");
        let boot_args = format!(
            "console=ttyS0 reboot=k panic=1 pci=off init=/sbin/init rustbox.ip={guest_ip}/30 rustbox.gw={host_ip}"
        );
        let mac = format!("02:FC:00:00:{:02X}:{:02X}", cid / 256, cid % 256);

        client
            .put_boot_source("/opt/rustbox/images/vmlinux", &boot_args)
            .await?;

        client
            .put_drive("rootfs", "/opt/rustbox/images/rootfs.ext4", true, false)
            .await?;

        client.put_machine_config(vcpu_count, mem_size_mib).await?;

        // 6. Setup TAP networking inside Lima for this microVM.
        // Must happen before put_network_interface so the TAP device exists
        // when Firecracker tries to open it.
        ssh.exec(&format!(
            "sudo ip link del {tap_name} 2>/dev/null; \
             sudo ip tuntap add dev {tap_name} mode tap && \
             sudo ip addr add {host_ip}/30 dev {tap_name} && \
             sudo ip link set dev {tap_name} up"
        ))
        .await?;

        client
            .put_network_interface("eth0", &tap_name, &mac)
            .await?;

        let vsock_path = format!("/tmp/fc-{id}.vsock");
        client.put_vsock(&vsock_path, cid).await?;

        client.start_instance().await?;

        // 7. Start SSH forward for the guest agent (TCP).
        let agent_forward = ssh
            .forward_tcp(agent_port, &guest_ip, AGENT_GUEST_PORT)
            .await?;

        // Update instance state.
        if let Some(mut inst) = self.instances.get_mut(&key) {
            inst.remote_pid = Some(remote_pid);
            inst.api_forward = Some(api_forward);
            inst.agent_forward = Some(agent_forward);
            inst.status = SandboxStatus::Running;
        }

        info!(%id, "lima sandbox started");
        Ok(())
    }

    async fn stop(&self, id: &SandboxId, blocking: bool) -> Result<()> {
        let key = id.to_string();
        info!(%id, blocking = blocking, "stopping lima sandbox");

        // Graceful stop via Firecracker API.
        let api_port = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            if inst.status != SandboxStatus::Running {
                return Err(RustboxError::SandboxNotRunning(key));
            }
            inst.api_local_port
        };

        let client = FirecrackerClient::new_tcp("127.0.0.1", api_port);
        if let Err(e) = client.stop_instance().await {
            warn!(%id, error = %e, "graceful stop failed");
        }

        // Kill remote Firecracker process and clean up.
        if let Some(mut inst) = self.instances.get_mut(&key) {
            inst.status = SandboxStatus::Stopping;

            // Kill Firecracker process inside Lima.
            if let Some(pid) = inst.remote_pid.take() {
                let _ = self.manager.ssh().kill_remote(pid).await;
            }

            // Kill SSH forward processes (they have kill_on_drop, but be explicit).
            if let Some(mut child) = inst.api_forward.take() {
                let _ = child.kill().await;
            }
            if let Some(mut child) = inst.agent_forward.take() {
                let _ = child.kill().await;
            }

            // Clean up TAP device inside Lima.
            let tap = inst.tap_name.clone();
            let socket = inst.remote_socket_path.clone();
            // Fire-and-forget cleanup.
            let ssh = self.manager.ssh();
            let _ = ssh
                .exec(&format!(
                    "sudo ip link del {tap} 2>/dev/null; rm -f {socket}"
                ))
                .await;

            inst.status = SandboxStatus::Stopped;
        }

        info!(%id, "lima sandbox stopped");
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
            if let Err(e) = agent.exec_streaming(request, cmd_id_clone, tx).await {
                tracing::error!(error = %e, "agent exec streaming failed");
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
        let api_port = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            if inst.status != SandboxStatus::Running {
                return Err(RustboxError::SandboxNotRunning(key));
            }
            inst.api_local_port
        };

        let snap_id = SnapshotId::new();
        let snapshot_path = format!("/tmp/snap-{id}-{snap_id}.snap");
        let mem_path = format!("/tmp/snap-{id}-{snap_id}.mem");

        let client = FirecrackerClient::new_tcp("127.0.0.1", api_port);
        client.pause_instance().await?;
        client.create_snapshot(&snapshot_path, &mem_path).await?;

        Ok(snap_id)
    }

    async fn snapshot_restore(
        &self,
        id: &SandboxId,
        snap: &SnapshotId,
    ) -> Result<()> {
        let key = id.to_string();
        let api_port = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            inst.api_local_port
        };

        let snapshot_path = format!("/tmp/snap-{id}-{snap}.snap");
        let mem_path = format!("/tmp/snap-{id}-{snap}.mem");

        // Verify snapshot exists inside Lima.
        let check = self
            .manager
            .ssh()
            .exec(&format!("test -f {snapshot_path} && echo EXISTS || echo MISSING"))
            .await?;

        if check.trim() != "EXISTS" {
            return Err(RustboxError::SnapshotNotFound(snap.to_string()));
        }

        let client = FirecrackerClient::new_tcp("127.0.0.1", api_port);
        client.load_snapshot(&snapshot_path, &mem_path).await?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn backend_creates_with_default_config() {
        let backend = LimaFirecrackerBackend::new(LimaConfig::default()).await.unwrap();
        // CID starts at 3.
        assert_eq!(backend.allocate_cid(), 3);
        assert_eq!(backend.allocate_cid(), 4);
    }

    #[test]
    fn guest_ip_formatting() {
        assert_eq!(LimaFirecrackerBackend::guest_ip(3), "172.16.3.2");
        assert_eq!(LimaFirecrackerBackend::guest_ip(10), "172.16.10.2");
        assert_eq!(LimaFirecrackerBackend::guest_ip(255), "172.16.255.2");
    }

    #[tokio::test]
    async fn port_allocation_increments() {
        let backend = LimaFirecrackerBackend::new(LimaConfig::default()).await.unwrap();
        let p1 = backend.allocate_port();
        let p2 = backend.allocate_port();
        assert_ne!(p1, p2, "each allocation should return a unique port");
        assert!(p1 > 0 && p2 > 0);
    }
}
