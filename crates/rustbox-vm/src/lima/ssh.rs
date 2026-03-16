use rustbox_core::{Result, RustboxError};
use std::process::Stdio;
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tracing::{debug, warn};

/// Executes commands inside a Lima VM instance via `limactl shell`.
pub struct SshExecutor {
    instance_name: String,
}

impl SshExecutor {
    pub fn new(instance_name: &str) -> Self {
        Self {
            instance_name: instance_name.to_string(),
        }
    }

    /// Run a command inside the Lima VM, returning stdout.
    pub async fn exec(&self, command: &str) -> Result<String> {
        debug!(instance = %self.instance_name, cmd = %command, "lima exec");

        let output = Command::new("limactl")
            .args(["shell", &self.instance_name, "bash", "-c", command])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| RustboxError::VmBackend(format!("limactl shell exec: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RustboxError::VmBackend(format!(
                "lima exec failed (exit {}): {stderr}",
                output.status.code().unwrap_or(-1)
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Run a command in the background inside Lima, returning the remote PID.
    pub async fn spawn_background(&self, command: &str) -> Result<u32> {
        let wrapped = format!("nohup {command} > /dev/null 2>&1 & echo $!");
        let output = self.exec(&wrapped).await?;
        let pid: u32 = output.trim().parse().map_err(|e| {
            RustboxError::VmBackend(format!("failed to parse background PID: {e} (got: {output})"))
        })?;
        debug!(pid = pid, "spawned background process in lima");
        Ok(pid)
    }

    /// Kill a process inside the Lima VM by PID.
    pub async fn kill_remote(&self, pid: u32) -> Result<()> {
        debug!(pid = pid, "killing remote process");
        // Use kill -9 and ignore errors (process may already be dead).
        let result = self.exec(&format!("kill -9 {pid} 2>/dev/null || true")).await;
        if let Err(e) = &result {
            warn!(pid = pid, error = %e, "kill_remote failed (may already be dead)");
        }
        Ok(())
    }

    /// Start an SSH port-forward that maps a local TCP port to a remote Unix socket inside Lima.
    ///
    /// Uses Lima's SSH config to establish: `127.0.0.1:<local_port> -> <remote_socket>`.
    /// Returns the child process handle (caller must keep it alive).
    pub async fn forward_unix_socket(
        &self,
        local_port: u16,
        remote_socket: &str,
    ) -> Result<Child> {
        debug!(
            local_port = local_port,
            remote_socket = %remote_socket,
            "starting unix socket forward"
        );

        let (config_path, destination) = self.get_ssh_config().await?;

        // Disable ControlMaster so this SSH process stays alive and owns the
        // port forwarding. With multiplexing, the forwarding would be torn down
        // when the slave exits after handing off to the master.
        let mut child = Command::new("ssh")
            .args(["-F", &config_path])
            .args(["-o", "ControlPath=none"])
            .arg("-N")
            .arg("-L")
            .arg(format!("127.0.0.1:{local_port}:{remote_socket}"))
            .arg(&destination)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| RustboxError::VmBackend(format!("spawn ssh forward: {e}")))?;

        Self::wait_for_tunnel(&mut child, local_port).await?;

        Ok(child)
    }

    /// Start an SSH port-forward that maps a local TCP port to a remote TCP port inside Lima.
    pub async fn forward_tcp(
        &self,
        local_port: u16,
        remote_host: &str,
        remote_port: u16,
    ) -> Result<Child> {
        debug!(
            local_port = local_port,
            remote_host = %remote_host,
            remote_port = remote_port,
            "starting tcp forward"
        );

        let (config_path, destination) = self.get_ssh_config().await?;

        let mut child = Command::new("ssh")
            .args(["-F", &config_path])
            .args(["-o", "ControlPath=none"])
            .arg("-N")
            .arg("-L")
            .arg(format!(
                "127.0.0.1:{local_port}:{remote_host}:{remote_port}"
            ))
            .arg(&destination)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| RustboxError::VmBackend(format!("spawn ssh tcp forward: {e}")))?;

        Self::wait_for_tunnel(&mut child, local_port).await?;

        Ok(child)
    }

    /// Poll `127.0.0.1:{local_port}` until a TCP connection succeeds, or timeout after 5s.
    ///
    /// Checks for early SSH exit each iteration so we fail fast with diagnostics
    /// instead of waiting the full timeout.
    async fn wait_for_tunnel(child: &mut Child, local_port: u16) -> Result<()> {
        let addr = format!("127.0.0.1:{local_port}");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let interval = std::time::Duration::from_millis(100);

        loop {
            // Check if SSH exited early.
            match child.try_wait() {
                Ok(Some(status)) => {
                    let stderr = if let Some(ref mut se) = child.stderr {
                        let mut buf = Vec::new();
                        use tokio::io::AsyncReadExt;
                        let _ = se.read_to_end(&mut buf).await;
                        String::from_utf8_lossy(&buf).to_string()
                    } else {
                        String::new()
                    };
                    return Err(RustboxError::VmBackend(format!(
                        "ssh tunnel exited early (exit {status}): {stderr}"
                    )));
                }
                Ok(None) => {} // still running
                Err(e) => {
                    return Err(RustboxError::VmBackend(format!(
                        "failed to check ssh tunnel status: {e}"
                    )));
                }
            }

            // Try to connect.
            if TcpStream::connect(&addr).await.is_ok() {
                debug!(port = local_port, "ssh tunnel ready");
                return Ok(());
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(RustboxError::Timeout(format!(
                    "ssh tunnel to {addr} not ready after 5s"
                )));
            }

            tokio::time::sleep(interval).await;
        }
    }

    /// Get the SSH config file path and destination host for this Lima instance.
    ///
    /// Returns `(config_path, destination)` where destination is `lima-<instance>`.
    /// Uses `ssh -F <config> ... <destination>` which is the recommended approach
    /// (Lima marks `show-ssh` as deprecated).
    async fn get_ssh_config(&self) -> Result<(String, String)> {
        let output = Command::new("limactl")
            .args(["ls", "--format={{.SSHConfigFile}}", &self.instance_name])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| RustboxError::VmBackend(format!("limactl ls: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(RustboxError::VmBackend(format!(
                "limactl ls failed: {stderr}"
            )));
        }

        let config_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if config_path.is_empty() {
            return Err(RustboxError::VmBackend(
                "limactl returned empty SSH config path".to_string(),
            ));
        }

        let destination = format!("lima-{}", self.instance_name);
        Ok((config_path, destination))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_executor_stores_instance_name() {
        let exec = SshExecutor::new("rustbox");
        assert_eq!(exec.instance_name, "rustbox");
    }

    #[test]
    fn ssh_executor_custom_name() {
        let exec = SshExecutor::new("my-custom-vm");
        assert_eq!(exec.instance_name, "my-custom-vm");
    }
}
