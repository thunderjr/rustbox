use std::collections::HashMap;

use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions,
    RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
};
use bollard::image::CommitContainerOptions;
use bollard::models::{HostConfig, PortBinding, PortMap};
use tokio_stream::StreamExt;
use bollard::Docker;
use dashmap::DashMap;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use rustbox_core::network::NetworkMode;
use rustbox_core::protocol::{AgentRequest, AgentResponse};
use rustbox_core::{
    backend::VmBackend, CommandId, CommandOutput, CommandRequest, Result, RustboxError,
    SandboxConfig, SandboxId, SandboxMetrics, SandboxStatus, SnapshotId,
};

use super::images::image_for_runtime;
use crate::agent_client::AgentClient;

/// Agent port inside the container.
const AGENT_PORT: u16 = 5123;

/// Label used to identify containers managed by rustbox.
const MANAGED_LABEL: &str = "rustbox.managed";

/// Label storing the sandbox ID on a container.
const SANDBOX_ID_LABEL: &str = "rustbox.sandbox_id";

/// Configuration for the Docker backend.
pub struct DockerBackendConfig {
    /// Override Docker socket URI (default: auto-detect).
    pub docker_host: Option<String>,
    /// Image name prefix (default: "rustbox").
    pub image_prefix: String,
}

impl Default for DockerBackendConfig {
    fn default() -> Self {
        Self {
            docker_host: None,
            image_prefix: "rustbox".to_string(),
        }
    }
}

/// Per-sandbox container state.
struct DockerInstance {
    config: SandboxConfig,
    status: SandboxStatus,
    container_id: String,
    agent_host_port: u16,
}

/// VmBackend implementation backed by Docker containers.
///
/// Each sandbox runs as a Docker container with the `rustbox-agent` binary
/// as its entrypoint. The agent communicates over TCP using the same
/// length-prefixed JSON protocol as the Firecracker backend.
pub struct DockerBackend {
    docker: Docker,
    config: DockerBackendConfig,
    instances: DashMap<String, DockerInstance>,
}

impl DockerBackend {
    /// Create a new Docker backend, connecting to the Docker daemon.
    /// Also cleans up any orphaned rustbox containers from previous runs.
    pub async fn new(config: DockerBackendConfig) -> std::result::Result<Self, RustboxError> {
        let docker = match &config.docker_host {
            Some(host) => Docker::connect_with_socket(host, 120, bollard::API_DEFAULT_VERSION)
                .map_err(|e| RustboxError::VmBackend(format!("connect to Docker at {host}: {e}")))?,
            None => Docker::connect_with_local_defaults()
                .map_err(|e| RustboxError::VmBackend(format!("connect to Docker: {e}")))?,
        };

        // Verify Docker is reachable.
        docker
            .ping()
            .await
            .map_err(|e| RustboxError::VmBackend(format!("Docker ping failed: {e}")))?;

        let backend = Self {
            docker,
            config,
            instances: DashMap::new(),
        };

        backend.cleanup_orphaned_containers().await;

        Ok(backend)
    }

    /// Remove any stopped/exited containers with the `rustbox.managed` label.
    async fn cleanup_orphaned_containers(&self) {
        let mut filters = HashMap::new();
        filters.insert("label", vec![MANAGED_LABEL]);
        filters.insert("status", vec!["exited", "dead", "created"]);

        let opts = ListContainersOptions {
            all: true,
            filters,
            ..Default::default()
        };

        match self.docker.list_containers(Some(opts)).await {
            Ok(containers) => {
                for container in containers {
                    if let Some(id) = container.id {
                        info!(container_id = %id, "removing orphaned rustbox container");
                        let opts = RemoveContainerOptions {
                            force: true,
                            ..Default::default()
                        };
                        if let Err(e) = self.docker.remove_container(&id, Some(opts)).await {
                            warn!(container_id = %id, error = %e, "failed to remove orphaned container");
                        }
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "failed to list orphaned containers");
            }
        }
    }

    fn agent_client_for(&self, instance: &DockerInstance) -> AgentClient {
        AgentClient::new_tcp("127.0.0.1".to_string(), instance.agent_host_port)
    }

    /// Poll the agent's Ping/Pong until it responds, or timeout.
    async fn wait_for_agent(host: &str, port: u16, timeout_secs: u64) -> Result<()> {
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(timeout_secs);

        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(RustboxError::Timeout(format!(
                    "agent did not respond on {host}:{port} within {timeout_secs}s"
                )));
            }

            let client = AgentClient::new_tcp(host.to_string(), port);
            if let Ok(mut conn) = client.connect().await {
                if conn.send_request(&AgentRequest::Ping).await.is_ok() {
                    if let Ok(resp) = conn.recv_response().await {
                        if matches!(resp, AgentResponse::Pong) {
                            return Ok(());
                        }
                    }
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// Inspect a running container to find the host-mapped port for the agent.
    async fn get_mapped_port(&self, container_id: &str) -> Result<u16> {
        let info = self
            .docker
            .inspect_container(container_id, None)
            .await
            .map_err(|e| RustboxError::VmBackend(format!("inspect container: {e}")))?;

        let ports = info
            .network_settings
            .as_ref()
            .and_then(|ns| ns.ports.as_ref())
            .ok_or_else(|| RustboxError::VmBackend("no port mappings found".to_string()))?;

        let key = format!("{AGENT_PORT}/tcp");
        let bindings = ports
            .get(&key)
            .and_then(|b| b.as_ref())
            .ok_or_else(|| {
                RustboxError::VmBackend(format!("no binding for {key}"))
            })?;

        let binding = bindings
            .first()
            .ok_or_else(|| RustboxError::VmBackend("empty port binding list".to_string()))?;

        binding
            .host_port
            .as_ref()
            .and_then(|p| p.parse::<u16>().ok())
            .ok_or_else(|| RustboxError::VmBackend("invalid host port".to_string()))
    }
}

#[async_trait]
impl VmBackend for DockerBackend {
    async fn create(&self, id: &SandboxId, config: &SandboxConfig) -> Result<()> {
        let key = id.to_string();
        let image = image_for_runtime(&config.runtime, &self.config.image_prefix);

        info!(%id, image = %image, "creating docker sandbox");

        // Ensure image exists locally. If missing, attempt a pull only when the
        // image name looks like a registry reference (contains '/').  Local-only
        // images (e.g. "rustbox-node24:latest") are expected to have been built
        // ahead of time via the Dockerfiles in images/.
        if self.docker.inspect_image(&image).await.is_err() {
            if image.contains('/') {
                use bollard::image::CreateImageOptions;

                info!(%id, image = %image, "pulling image");
                let opts = CreateImageOptions {
                    from_image: image.clone(),
                    ..Default::default()
                };
                let mut stream = self.docker.create_image(Some(opts), None, None);
                while let Some(result) = stream.next().await {
                    if let Err(e) = result {
                        return Err(RustboxError::VmBackend(format!("pull image {image}: {e}")));
                    }
                }
            } else {
                return Err(RustboxError::VmBackend(format!(
                    "image {image} not found locally — build it with: \
                     docker build -t {image} images/<runtime>/"
                )));
            }
        }

        // Build container config.
        let cpu_count = config.cpu_count as u64;
        let nano_cpus = cpu_count * 1_000_000_000;
        let memory = std::cmp::max(512, cpu_count * 256) as i64 * 1024 * 1024;

        let mut env_vars: Vec<String> = config
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        // Always tell the agent which port to listen on.
        env_vars.push(format!("RUSTBOX_AGENT_PORT={AGENT_PORT}"));

        let exposed_port_key = format!("{AGENT_PORT}/tcp");
        let mut exposed_ports = HashMap::new();
        exposed_ports.insert(exposed_port_key.clone(), HashMap::new());

        let mut port_bindings: PortMap = HashMap::new();
        port_bindings.insert(
            exposed_port_key,
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some("0".to_string()), // OS-assigned
            }]),
        );

        let network_mode = match config.network_policy.mode {
            NetworkMode::DenyAll => Some("none".to_string()),
            NetworkMode::AllowAll => None, // default bridge
        };

        let mut labels = HashMap::new();
        labels.insert(MANAGED_LABEL.to_string(), "true".to_string());
        labels.insert(SANDBOX_ID_LABEL.to_string(), key.clone());

        let host_config = HostConfig {
            nano_cpus: Some(nano_cpus as i64),
            memory: Some(memory),
            port_bindings: Some(port_bindings),
            network_mode,
            ..Default::default()
        };

        let container_config = Config {
            image: Some(image.clone()),
            env: Some(env_vars),
            exposed_ports: Some(exposed_ports),
            labels: Some(labels),
            host_config: Some(host_config),
            entrypoint: Some(vec!["/usr/bin/rustbox-agent".to_string()]),
            ..Default::default()
        };

        let opts = CreateContainerOptions {
            name: format!("rustbox-{key}"),
            ..Default::default()
        };

        let response = self
            .docker
            .create_container(Some(opts), container_config)
            .await
            .map_err(|e| RustboxError::VmBackend(format!("create container: {e}")))?;

        self.instances.insert(
            key,
            DockerInstance {
                config: config.clone(),
                status: SandboxStatus::Pending,
                container_id: response.id,
                agent_host_port: 0, // set on start
            },
        );

        Ok(())
    }

    async fn start(&self, id: &SandboxId) -> Result<()> {
        let key = id.to_string();
        let container_id = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            inst.container_id.clone()
        };

        info!(%id, container = %container_id, "starting docker sandbox");

        self.docker
            .start_container(&container_id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| RustboxError::VmBackend(format!("start container: {e}")))?;

        // Discover the mapped agent port.
        let host_port = self.get_mapped_port(&container_id).await?;
        debug!(%id, host_port = host_port, "agent port mapped");

        // Wait for the agent to be ready.
        Self::wait_for_agent("127.0.0.1", host_port, 30).await?;

        // Update instance state.
        if let Some(mut inst) = self.instances.get_mut(&key) {
            inst.agent_host_port = host_port;
            inst.status = SandboxStatus::Running;
        }

        info!(%id, "docker sandbox started");
        Ok(())
    }

    async fn stop(&self, id: &SandboxId, _blocking: bool) -> Result<()> {
        let key = id.to_string();
        let container_id = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            if inst.status != SandboxStatus::Running {
                return Err(RustboxError::SandboxNotRunning(key));
            }
            inst.container_id.clone()
        };

        info!(%id, "stopping docker sandbox");

        let stop_opts = StopContainerOptions { t: 10 };
        if let Err(e) = self.docker.stop_container(&container_id, Some(stop_opts)).await {
            warn!(%id, error = %e, "graceful stop failed, force removing");
        }

        let remove_opts = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        if let Err(e) = self.docker.remove_container(&container_id, Some(remove_opts)).await {
            warn!(%id, error = %e, "failed to remove container");
        }

        if let Some(mut inst) = self.instances.get_mut(&key) {
            inst.status = SandboxStatus::Stopped;
        }

        info!(%id, "docker sandbox stopped");
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
            AgentResponse::FileContent { data } => Ok(data),
            AgentResponse::Error { message } => Err(RustboxError::AgentComm(message)),
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
        let container_id = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            if inst.status != SandboxStatus::Running {
                return Err(RustboxError::SandboxNotRunning(key));
            }
            inst.container_id.clone()
        };

        let snap_id = SnapshotId::new();
        let repo = format!("rustbox-snap-{key}");
        let tag = snap_id.to_string();

        info!(%id, snap = %snap_id, "creating docker snapshot");

        let opts = CommitContainerOptions {
            container: container_id,
            repo,
            tag,
            ..Default::default()
        };

        self.docker
            .commit_container(opts, Config::<String>::default())
            .await
            .map_err(|e| RustboxError::VmBackend(format!("commit container: {e}")))?;

        Ok(snap_id)
    }

    async fn snapshot_restore(
        &self,
        id: &SandboxId,
        snap: &SnapshotId,
    ) -> Result<()> {
        let key = id.to_string();

        // Get original config and stop current container.
        let (original_config, old_container_id) = {
            let inst = self
                .instances
                .get(&key)
                .ok_or_else(|| RustboxError::SandboxNotFound(key.clone()))?;
            (inst.config.clone(), inst.container_id.clone())
        };

        info!(%id, snap = %snap, "restoring docker snapshot");

        // Stop and remove old container.
        let stop_opts = StopContainerOptions { t: 5 };
        let _ = self.docker.stop_container(&old_container_id, Some(stop_opts)).await;
        let remove_opts = RemoveContainerOptions {
            force: true,
            ..Default::default()
        };
        let _ = self.docker.remove_container(&old_container_id, Some(remove_opts)).await;

        // Create new container from snapshot image.
        let snapshot_image = format!("rustbox-snap-{key}:{snap}");

        // Verify the snapshot image exists.
        self.docker
            .inspect_image(&snapshot_image)
            .await
            .map_err(|_| RustboxError::SnapshotNotFound(snap.to_string()))?;

        let cpu_count = original_config.cpu_count as u64;
        let nano_cpus = cpu_count * 1_000_000_000;
        let memory = std::cmp::max(512, cpu_count * 256) as i64 * 1024 * 1024;

        let mut env_vars: Vec<String> = original_config
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        env_vars.push(format!("RUSTBOX_AGENT_PORT={AGENT_PORT}"));

        let exposed_port_key = format!("{AGENT_PORT}/tcp");
        let mut exposed_ports = HashMap::new();
        exposed_ports.insert(exposed_port_key.clone(), HashMap::new());

        let mut port_bindings: PortMap = HashMap::new();
        port_bindings.insert(
            exposed_port_key,
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some("0".to_string()),
            }]),
        );

        let network_mode = match original_config.network_policy.mode {
            NetworkMode::DenyAll => Some("none".to_string()),
            NetworkMode::AllowAll => None,
        };

        let mut labels = HashMap::new();
        labels.insert(MANAGED_LABEL.to_string(), "true".to_string());
        labels.insert(SANDBOX_ID_LABEL.to_string(), key.clone());

        let host_config = HostConfig {
            nano_cpus: Some(nano_cpus as i64),
            memory: Some(memory),
            port_bindings: Some(port_bindings),
            network_mode,
            ..Default::default()
        };

        let container_config = Config {
            image: Some(snapshot_image),
            env: Some(env_vars),
            exposed_ports: Some(exposed_ports),
            labels: Some(labels),
            host_config: Some(host_config),
            entrypoint: Some(vec!["/usr/bin/rustbox-agent".to_string()]),
            ..Default::default()
        };

        let opts = CreateContainerOptions {
            name: format!("rustbox-{key}"),
            ..Default::default()
        };

        let response = self
            .docker
            .create_container(Some(opts), container_config)
            .await
            .map_err(|e| RustboxError::VmBackend(format!("create restored container: {e}")))?;

        let new_container_id = response.id;

        self.docker
            .start_container(&new_container_id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| RustboxError::VmBackend(format!("start restored container: {e}")))?;

        let host_port = self.get_mapped_port(&new_container_id).await?;
        Self::wait_for_agent("127.0.0.1", host_port, 30).await?;

        if let Some(mut inst) = self.instances.get_mut(&key) {
            inst.container_id = new_container_id;
            inst.agent_host_port = host_port;
            inst.status = SandboxStatus::Running;
        }

        info!(%id, "docker snapshot restored");
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
            AgentResponse::MetricsResult(m) => Ok(m),
            AgentResponse::Error { message } => Err(RustboxError::AgentComm(message)),
            other => Err(RustboxError::AgentComm(format!(
                "unexpected response: {other:?}"
            ))),
        }
    }
}
