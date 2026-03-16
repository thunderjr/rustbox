use rustbox_core::{Result, RustboxError};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::process::Command;
use tracing::{debug, info, warn};

use super::ssh::SshExecutor;

/// Configuration for the Lima VM that hosts Firecracker.
#[derive(Clone, Debug)]
pub struct LimaConfig {
    /// Lima instance name. Default: "rustbox".
    pub instance_name: String,
    /// Number of CPUs for the Lima VM. Default: 4.
    pub cpus: u32,
    /// Memory in GiB for the Lima VM. Default: 4.
    pub memory_gib: u32,
    /// Disk size in GiB. Default: 20.
    pub disk_gib: u32,
    /// Firecracker release version to install inside Lima. Default: "v1.13.0".
    pub firecracker_version: String,
}

impl Default for LimaConfig {
    fn default() -> Self {
        Self {
            instance_name: "rustbox".to_string(),
            cpus: 4,
            memory_gib: 4,
            disk_gib: 20,
            firecracker_version: "v1.13.0".to_string(),
        }
    }
}

/// Lima instance status as reported by `limactl list --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LimaInstanceStatus {
    Running,
    Stopped,
    NotFound,
}

/// Manages the Lima VM lifecycle and one-time provisioning of Firecracker
/// and its dependencies inside the VM.
pub struct LimaManager {
    config: LimaConfig,
    ssh: SshExecutor,
    provisioned: AtomicBool,
}

impl LimaManager {
    pub fn new(config: LimaConfig) -> Self {
        let ssh = SshExecutor::new(&config.instance_name);
        Self {
            config,
            ssh,
            provisioned: AtomicBool::new(false),
        }
    }

    pub fn ssh(&self) -> &SshExecutor {
        &self.ssh
    }

    pub fn config(&self) -> &LimaConfig {
        &self.config
    }

    /// Ensure the Lima VM is running and provisioned with Firecracker.
    /// Idempotent — safe to call on every sandbox create.
    pub async fn ensure_ready(&self) -> Result<()> {
        // 1. Check limactl is available.
        self.check_limactl().await?;

        // 2. Check instance status.
        let status = self.instance_status().await?;

        match status {
            LimaInstanceStatus::NotFound => {
                info!(
                    instance = %self.config.instance_name,
                    "creating Lima VM with nested virtualization"
                );
                self.create_instance().await?;
                self.start_instance().await?;
            }
            LimaInstanceStatus::Stopped => {
                info!(
                    instance = %self.config.instance_name,
                    "starting stopped Lima VM"
                );
                self.start_instance().await?;
            }
            LimaInstanceStatus::Running => {
                debug!(
                    instance = %self.config.instance_name,
                    "Lima VM already running"
                );
            }
        }

        // 3. Provision if needed.
        if !self.provisioned.load(Ordering::Relaxed) {
            self.provision_if_needed().await?;
        }

        // 4. Ensure /dev/kvm is accessible (permissions don't persist across VM restarts).
        self.ssh
            .exec("sudo chmod 666 /dev/kvm 2>/dev/null || true")
            .await?;

        Ok(())
    }

    /// Check that `limactl` is on PATH.
    async fn check_limactl(&self) -> Result<()> {
        let output = Command::new("which")
            .arg("limactl")
            .output()
            .await
            .map_err(|e| {
                RustboxError::VmBackend(format!(
                    "failed to check for limactl: {e}. Install with: brew install lima"
                ))
            })?;

        if !output.status.success() {
            return Err(RustboxError::VmBackend(
                "limactl not found on PATH. Install with: brew install lima".to_string(),
            ));
        }
        Ok(())
    }

    /// Query the Lima instance status.
    pub async fn instance_status(&self) -> Result<LimaInstanceStatus> {
        let output = Command::new("limactl")
            .args(["list", "--json"])
            .output()
            .await
            .map_err(|e| RustboxError::VmBackend(format!("limactl list: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);

        // limactl list --json outputs one JSON object per line.
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                if v.get("name").and_then(|n| n.as_str()) == Some(&self.config.instance_name) {
                    let status = v
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("Unknown");
                    return Ok(match status {
                        "Running" => LimaInstanceStatus::Running,
                        "Stopped" => LimaInstanceStatus::Stopped,
                        _ => {
                            warn!(
                                status = status,
                                "unexpected Lima instance status, treating as stopped"
                            );
                            LimaInstanceStatus::Stopped
                        }
                    });
                }
            }
        }

        Ok(LimaInstanceStatus::NotFound)
    }

    /// Create a new Lima instance with nested virtualization enabled.
    async fn create_instance(&self) -> Result<()> {
        let output = Command::new("limactl")
            .args([
                "create",
                "--set",
                ".nestedVirtualization=true",
                &format!("--set=.cpus={}", self.config.cpus),
                &format!(
                    "--set=.memory=\"{}GiB\"",
                    self.config.memory_gib
                ),
                &format!(
                    "--set=.disk=\"{}GiB\"",
                    self.config.disk_gib
                ),
                &format!("--name={}", self.config.instance_name),
                "--tty=false",
                "template://default",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| RustboxError::VmBackend(format!("limactl create: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RustboxError::VmBackend(format!(
                "limactl create failed: {stderr}"
            )));
        }

        info!(instance = %self.config.instance_name, "Lima instance created");
        Ok(())
    }

    /// Start an existing Lima instance.
    async fn start_instance(&self) -> Result<()> {
        let output = Command::new("limactl")
            .args(["start", &self.config.instance_name])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| RustboxError::VmBackend(format!("limactl start: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RustboxError::VmBackend(format!(
                "limactl start failed: {stderr}"
            )));
        }

        info!(instance = %self.config.instance_name, "Lima instance started");
        Ok(())
    }

    /// Check if Firecracker is already provisioned; if not, install it.
    async fn provision_if_needed(&self) -> Result<()> {
        let marker = "/home/*.linux/.rustbox-provisioned";
        let check = self
            .ssh
            .exec(&format!("ls {marker} 2>/dev/null && echo EXISTS || echo MISSING"))
            .await?;

        if check.trim().contains("EXISTS") {
            info!("Lima VM already provisioned");
            self.provisioned.store(true, Ordering::Relaxed);
            return Ok(());
        }

        info!("provisioning Lima VM with Firecracker and dependencies...");
        self.provision().await?;
        self.provisioned.store(true, Ordering::Relaxed);
        Ok(())
    }

    /// Full provisioning: download Firecracker, kernel, rootfs, setup networking.
    async fn provision(&self) -> Result<()> {
        let ver = &self.config.firecracker_version;

        // 1. Build the agent binary via Docker on macOS host (linux/arm64 native).
        self.build_agent_docker().await?;
        info!("built agent binary via Docker");

        // 2. Download and install Firecracker binary inside Lima.
        let fc_url = format!(
            "https://github.com/firecracker-microvm/firecracker/releases/download/{ver}/firecracker-{ver}-aarch64.tgz"
        );
        self.ssh.exec(&format!(
            "cd /tmp && \
             curl -sL '{fc_url}' -o firecracker.tgz && \
             tar xzf firecracker.tgz && \
             sudo mv release-{ver}-aarch64/firecracker-{ver}-aarch64 /usr/local/bin/firecracker && \
             sudo chmod +x /usr/local/bin/firecracker && \
             rm -rf firecracker.tgz release-{ver}-aarch64"
        )).await?;
        info!("installed Firecracker {ver}");

        // 3. Download kernel from Firecracker CI S3 (5.10.x, aarch64).
        self.ssh.exec(
            "sudo mkdir -p /opt/rustbox/images && \
             cd /opt/rustbox/images && \
             [ -f vmlinux ] || sudo curl -sL \
               'https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.13/aarch64/vmlinux-5.10.239' \
               -o vmlinux"
        ).await?;
        info!("downloaded kernel image");

        // 4. Download Ubuntu 24.04 squashfs rootfs from Firecracker CI S3.
        self.ssh.exec(
            "cd /opt/rustbox/images && \
             [ -f ubuntu-24.04.squashfs ] || sudo curl -sL \
               'https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.13/aarch64/ubuntu-24.04.squashfs' \
               -o ubuntu-24.04.squashfs"
        ).await?;
        info!("downloaded Ubuntu 24.04 squashfs rootfs");

        // 5. Convert squashfs → ext4 and inject agent + systemd services (inside Lima).
        let workspace_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let agent_path = format!("{workspace_root}/target/linux/release/rustbox-agent");
        self.build_rootfs_image(&agent_path).await?;
        info!("built rootfs image with agent and systemd services");

        // 6. Ensure /dev/kvm permissions.
        self.ssh
            .exec("sudo chmod 666 /dev/kvm 2>/dev/null || true")
            .await?;

        // 7. Setup bridge networking for microVM connectivity.
        self.ssh.exec(
            "sudo ip link show br0 >/dev/null 2>&1 || { \
               sudo ip link add br0 type bridge && \
               sudo ip addr add 172.16.0.1/16 dev br0 && \
               sudo ip link set br0 up && \
               sudo sysctl -w net.ipv4.ip_forward=1 >/dev/null && \
               sudo iptables -t nat -A POSTROUTING -o eth0 -j MASQUERADE && \
               sudo iptables -A FORWARD -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT && \
               sudo iptables -A FORWARD -i br0 -o eth0 -j ACCEPT; \
             }"
        ).await?;
        info!("bridge networking configured");

        // 8. Write marker file.
        self.ssh
            .exec("touch $HOME/.rustbox-provisioned")
            .await?;

        info!("Lima VM provisioning complete");
        Ok(())
    }

    /// Build the rustbox-agent binary via Docker on the macOS host.
    ///
    /// On Apple Silicon, `rust:latest` runs as native linux/arm64 — no emulation
    /// needed. The output lands on the macOS filesystem at `target/linux/release/`.
    async fn build_agent_docker(&self) -> Result<()> {
        let workspace_root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

        let output = Command::new("docker")
            .args([
                "run", "--rm",
                "-v", &format!("{workspace_root}:/src"),
                "-w", "/src",
                "rust:latest",
                "cargo", "build", "--release",
                "-p", "rustbox-agent",
                "--target-dir", "/src/target/linux",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| RustboxError::VmBackend(format!(
                "failed to run docker for agent build: {e}. Is Docker installed and running?"
            )))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RustboxError::VmBackend(format!(
                "docker agent build failed: {stderr}"
            )));
        }

        // Verify the binary was produced.
        let agent_bin = format!("{workspace_root}/target/linux/release/rustbox-agent");
        let exists = Command::new("test")
            .args(["-f", &agent_bin])
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false);

        if !exists {
            return Err(RustboxError::VmBackend(format!(
                "agent binary not found at {agent_bin} after Docker build"
            )));
        }

        Ok(())
    }

    /// Convert the downloaded squashfs rootfs to ext4 and inject the agent binary
    /// and systemd service files. This runs inside Lima because macOS cannot
    /// mount ext4/squashfs filesystems.
    async fn build_rootfs_image(&self, agent_path: &str) -> Result<()> {
        // Install squashfs-tools if needed.
        self.ssh.exec(
            "which unsquashfs >/dev/null 2>&1 || \
             sudo apt-get update -qq && sudo apt-get install -y -qq squashfs-tools >/dev/null 2>&1"
        ).await?;

        // Convert squashfs → ext4, inject agent and systemd services.
        self.ssh.exec(&format!(
            "set -e && \
             MNT=/tmp/rootfs-mount && \
             sudo rm -rf /tmp/rootfs-squash $MNT && \
             sudo unsquashfs -d /tmp/rootfs-squash /opt/rustbox/images/ubuntu-24.04.squashfs && \
             dd if=/dev/zero of=/tmp/rootfs-new.ext4 bs=1M count=512 2>/dev/null && \
             mkfs.ext4 -qF /tmp/rootfs-new.ext4 && \
             sudo mkdir -p $MNT && \
             sudo mount -o loop /tmp/rootfs-new.ext4 $MNT && \
             sudo cp -a /tmp/rootfs-squash/* $MNT/ && \
             sudo cp {agent_path} $MNT/usr/bin/rustbox-agent && \
             sudo chmod +x $MNT/usr/bin/rustbox-agent && \
             printf '[Unit]\\nDescription=Rustbox Guest Agent\\nAfter=network.target\\n\\n\
[Service]\\nExecStart=/usr/bin/rustbox-agent\\nRestart=always\\n\\n\
[Install]\\nWantedBy=multi-user.target\\n' | sudo tee $MNT/etc/systemd/system/rustbox-agent.service > /dev/null && \
             sudo ln -sf ../rustbox-agent.service $MNT/etc/systemd/system/multi-user.target.wants/rustbox-agent.service && \
             printf '#!/bin/sh\\n\
IP=$(cat /proc/cmdline | tr \" \" \"\\n\" | grep \"^rustbox.ip=\" | cut -d= -f2)\\n\
GW=$(cat /proc/cmdline | tr \" \" \"\\n\" | grep \"^rustbox.gw=\" | cut -d= -f2)\\n\
[ -n \"$IP\" ] && ip addr add \"$IP\" dev eth0 && ip link set eth0 up\\n\
[ -n \"$GW\" ] && ip route add default via \"$GW\"\\n' | sudo tee $MNT/usr/bin/rustbox-net-setup > /dev/null && \
             sudo chmod +x $MNT/usr/bin/rustbox-net-setup && \
             printf '[Unit]\\nDescription=Rustbox Network Setup\\nBefore=rustbox-agent.service\\nAfter=systemd-networkd.service\\n\\n\
[Service]\\nType=oneshot\\nExecStart=/usr/bin/rustbox-net-setup\\nRemainAfterExit=yes\\n\\n\
[Install]\\nWantedBy=multi-user.target\\n' | sudo tee $MNT/etc/systemd/system/rustbox-network.service > /dev/null && \
             sudo ln -sf ../rustbox-network.service $MNT/etc/systemd/system/multi-user.target.wants/rustbox-network.service && \
             sudo umount $MNT && \
             sudo mv /tmp/rootfs-new.ext4 /opt/rustbox/images/rootfs.ext4 && \
             sudo rm -rf /tmp/rootfs-squash"
        )).await?;

        Ok(())
    }

    /// Stop the Lima VM.
    pub async fn stop(&self) -> Result<()> {
        let output = Command::new("limactl")
            .args(["stop", &self.config.instance_name])
            .output()
            .await
            .map_err(|e| RustboxError::VmBackend(format!("limactl stop: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(stderr = %stderr, "limactl stop returned non-zero");
        }

        self.provisioned.store(false, Ordering::Relaxed);
        Ok(())
    }

    /// Return the current status of the Lima instance.
    pub async fn status(&self) -> Result<LimaInstanceStatus> {
        self.instance_status().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = LimaConfig::default();
        assert_eq!(config.instance_name, "rustbox");
        assert_eq!(config.cpus, 4);
        assert_eq!(config.memory_gib, 4);
        assert_eq!(config.disk_gib, 20);
        assert_eq!(config.firecracker_version, "v1.13.0");
    }

    #[test]
    fn custom_config() {
        let config = LimaConfig {
            instance_name: "test-vm".into(),
            cpus: 8,
            memory_gib: 8,
            disk_gib: 50,
            firecracker_version: "v1.14.0".into(),
        };
        assert_eq!(config.instance_name, "test-vm");
        assert_eq!(config.cpus, 8);
    }

    #[test]
    fn manager_exposes_ssh_and_config() {
        let config = LimaConfig::default();
        let manager = LimaManager::new(config.clone());
        assert_eq!(manager.config().instance_name, "rustbox");
        // SshExecutor is accessible.
        let _ssh = manager.ssh();
    }
}
