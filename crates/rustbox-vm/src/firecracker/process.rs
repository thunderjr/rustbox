use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rustbox_core::{Result, RustboxError};
use tokio::process::{Child, Command};
use tokio::time::sleep;
use tracing::{debug, info, warn};

/// Configuration for spawning a Firecracker process.
#[derive(Clone, Debug)]
pub struct FirecrackerProcessConfig {
    /// Path to the firecracker binary. Defaults to "firecracker" (on PATH).
    pub firecracker_bin: PathBuf,
    /// Path where the process log is written.
    pub log_path: PathBuf,
}

impl Default for FirecrackerProcessConfig {
    fn default() -> Self {
        Self {
            firecracker_bin: PathBuf::from("firecracker"),
            log_path: PathBuf::from("/tmp/firecracker.log"),
        }
    }
}

/// Manages a running Firecracker process.
pub struct FirecrackerProcess {
    child: Child,
    socket_path: PathBuf,
    log_path: PathBuf,
}

impl FirecrackerProcess {
    /// Spawn a new Firecracker process listening on the given API socket path.
    pub async fn spawn(
        socket_path: &Path,
        config: &FirecrackerProcessConfig,
    ) -> Result<Self> {
        // Remove any stale socket file.
        let _ = std::fs::remove_file(socket_path);

        info!(
            bin = %config.firecracker_bin.display(),
            socket = %socket_path.display(),
            "spawning firecracker process"
        );

        let child = Command::new(&config.firecracker_bin)
            .arg("--api-sock")
            .arg(socket_path)
            .arg("--log-path")
            .arg(&config.log_path)
            .arg("--level")
            .arg("Warning")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                RustboxError::VmBackend(format!(
                    "failed to spawn firecracker ({}): {e}",
                    config.firecracker_bin.display()
                ))
            })?;

        // Wait for the API socket to appear.
        let timeout = Duration::from_secs(5);
        let poll_interval = Duration::from_millis(50);
        let start = std::time::Instant::now();

        loop {
            if socket_path.exists() {
                debug!("firecracker API socket ready");
                break;
            }
            if start.elapsed() >= timeout {
                return Err(RustboxError::Timeout(format!(
                    "firecracker API socket did not appear within {}s",
                    timeout.as_secs()
                )));
            }
            sleep(poll_interval).await;
        }

        Ok(Self {
            child,
            socket_path: socket_path.to_path_buf(),
            log_path: config.log_path.clone(),
        })
    }

    /// Kill the Firecracker process.
    pub async fn kill(&mut self) -> Result<()> {
        info!(
            socket = %self.socket_path.display(),
            "killing firecracker process"
        );
        self.child.kill().await.map_err(|e| {
            RustboxError::VmBackend(format!("failed to kill firecracker: {e}"))
        })?;
        let _ = self.child.wait().await;

        // Clean up the socket file.
        if self.socket_path.exists() {
            if let Err(e) = std::fs::remove_file(&self.socket_path) {
                warn!(
                    error = %e,
                    path = %self.socket_path.display(),
                    "failed to remove socket file"
                );
            }
        }

        Ok(())
    }

    /// Returns the API socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Returns the log path.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}

impl Drop for FirecrackerProcess {
    fn drop(&mut self) {
        // Best-effort cleanup. The `kill_on_drop(true)` on the child process
        // handle also ensures the process is killed.
        let _ = std::fs::remove_file(&self.socket_path);
    }
}
