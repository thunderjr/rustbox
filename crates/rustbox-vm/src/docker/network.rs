//! Networking helpers for Docker-backed sandboxes.
//!
//! Per-sandbox bridge network management and iptables rule application.
//! Network create/remove are cross-platform; iptables functions are Linux-only.

use bollard::Docker;
use bollard::network::{CreateNetworkOptions, ListNetworksOptions};
use std::collections::HashMap;
use tracing::{info, warn};

use rustbox_core::{Result, RustboxError, SandboxId};

/// Network name prefix for rustbox-managed Docker networks.
const NET_PREFIX: &str = "rustbox-net-";

/// Network name for a sandbox: "rustbox-net-{last 12 chars of id}".
///
/// Uses the tail of the UUID string to avoid collisions from UUIDv7's
/// timestamp prefix (IDs created in the same millisecond share a prefix).
pub fn network_name(id: &SandboxId) -> String {
    let s = id.to_string();
    let short = &s[s.len() - 12..];
    format!("{NET_PREFIX}{short}")
}

/// Create a per-sandbox Docker bridge network. Returns the network name.
pub async fn create_sandbox_network(docker: &Docker, id: &SandboxId) -> Result<String> {
    let name = network_name(id);

    info!(network = %name, "creating sandbox network");

    let opts = CreateNetworkOptions {
        name: name.clone(),
        driver: "bridge".to_string(),
        internal: false,
        ..Default::default()
    };

    docker
        .create_network(opts)
        .await
        .map_err(|e| RustboxError::VmBackend(format!("create network {name}: {e}")))?;

    Ok(name)
}

/// Remove a sandbox's Docker network (best-effort, logs warnings).
pub async fn remove_sandbox_network(docker: &Docker, id: &SandboxId) -> Result<()> {
    let name = network_name(id);

    info!(network = %name, "removing sandbox network");

    if let Err(e) = docker.remove_network(&name).await {
        warn!(network = %name, error = %e, "failed to remove sandbox network");
    }

    Ok(())
}

/// List and remove any orphaned `rustbox-net-*` Docker networks.
pub async fn cleanup_orphaned_networks(docker: &Docker) {
    let mut filters = HashMap::new();
    filters.insert("name".to_string(), vec![NET_PREFIX.to_string()]);

    let opts = ListNetworksOptions { filters };

    match docker.list_networks(Some(opts)).await {
        Ok(networks) => {
            for net in networks {
                if let Some(name) = &net.name {
                    if name.starts_with(NET_PREFIX) {
                        info!(network = %name, "removing orphaned rustbox network");
                        if let Err(e) = docker.remove_network(name).await {
                            warn!(network = %name, error = %e, "failed to remove orphaned network");
                        }
                    }
                }
            }
        }
        Err(e) => {
            warn!(error = %e, "failed to list networks for cleanup");
        }
    }
}

/// Apply iptables rules to DOCKER-USER chain for a container IP.
#[cfg(target_os = "linux")]
pub async fn apply_iptables(
    container_ip: &str,
    policy: &rustbox_core::network::NetworkPolicy,
) -> Result<()> {
    use rustbox_core::network::NetworkMode;

    // Always allow established/related connections back to the container.
    run_iptables(&[
        "-I", "DOCKER-USER",
        "-d", container_ip,
        "-m", "conntrack", "--ctstate", "ESTABLISHED,RELATED",
        "-j", "ACCEPT",
    ])
    .await?;

    match policy.mode {
        NetworkMode::AllowAll => {
            // Block specific denied subnets, allow everything else (Docker default).
            for subnet in &policy.subnets_deny {
                run_iptables(&[
                    "-A", "DOCKER-USER",
                    "-s", container_ip,
                    "-d", &subnet.to_string(),
                    "-j", "DROP",
                ])
                .await?;
            }
        }
        NetworkMode::DenyAll => {
            // Allow specific subnets, then drop everything else.
            for subnet in &policy.subnets_allow {
                run_iptables(&[
                    "-I", "DOCKER-USER",
                    "-s", container_ip,
                    "-d", &subnet.to_string(),
                    "-j", "ACCEPT",
                ])
                .await?;
            }
            // Drop all other outbound traffic from this container.
            run_iptables(&[
                "-A", "DOCKER-USER",
                "-s", container_ip,
                "-j", "DROP",
            ])
            .await?;
        }
    }

    Ok(())
}

/// Remove all DOCKER-USER rules referencing a container IP.
#[cfg(target_os = "linux")]
pub async fn remove_iptables(container_ip: &str) -> Result<()> {
    // List all rules in DOCKER-USER chain.
    let output = tokio::process::Command::new("iptables")
        .args(["-S", "DOCKER-USER"])
        .output()
        .await
        .map_err(|e| RustboxError::VmBackend(format!("iptables -S: {e}")))?;

    if !output.status.success() {
        // Chain might not exist, that's fine.
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains(container_ip) {
            // Convert "-A DOCKER-USER ..." to delete args.
            if let Some(rule_part) = line.strip_prefix("-A ") {
                let args: Vec<&str> = rule_part.split_whitespace().collect();
                let mut delete_args = vec!["-D"];
                delete_args.extend(&args);
                let _ = run_iptables(&delete_args).await;
            }
        }
    }

    Ok(())
}

/// Run a single iptables command.
#[cfg(target_os = "linux")]
async fn run_iptables(args: &[&str]) -> Result<()> {
    let output = tokio::process::Command::new("iptables")
        .args(args)
        .output()
        .await
        .map_err(|e| RustboxError::VmBackend(format!("iptables: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RustboxError::VmBackend(format!(
            "iptables {} failed: {stderr}",
            args.join(" ")
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_name_format() {
        let id = SandboxId::new();
        let name = network_name(&id);
        assert!(name.starts_with(NET_PREFIX));
        // 12 chars from id + prefix
        assert_eq!(name.len(), NET_PREFIX.len() + 12);
    }

    #[test]
    fn network_name_deterministic() {
        let id = SandboxId::new();
        assert_eq!(network_name(&id), network_name(&id));
    }

    #[test]
    fn network_name_different_ids_differ() {
        let id1 = SandboxId::new();
        let id2 = SandboxId::new();
        assert_ne!(network_name(&id1), network_name(&id2));
    }
}
