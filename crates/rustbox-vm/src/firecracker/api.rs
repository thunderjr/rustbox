use std::path::{Path, PathBuf};

use rustbox_core::{Result, RustboxError};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::{debug, warn};

/// Client for the Firecracker REST API over a Unix domain socket.
pub struct FirecrackerClient {
    socket_path: PathBuf,
}

impl FirecrackerClient {
    pub fn new(socket_path: &Path) -> Self {
        Self {
            socket_path: socket_path.to_path_buf(),
        }
    }

    /// Send a raw HTTP/1.1 request over the Unix socket and return (status_code, body).
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, String)> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|e| RustboxError::VmBackend(format!("connect to socket: {e}")))?;

        let body_bytes = body.unwrap_or("");
        let request = if body.is_some() {
            format!(
                "{method} {path} HTTP/1.1\r\n\
                 Host: localhost\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Accept: application/json\r\n\
                 Connection: close\r\n\
                 \r\n\
                 {body_bytes}",
                body_bytes.len()
            )
        } else {
            format!(
                "{method} {path} HTTP/1.1\r\n\
                 Host: localhost\r\n\
                 Accept: application/json\r\n\
                 Connection: close\r\n\
                 \r\n"
            )
        };

        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| RustboxError::VmBackend(format!("write request: {e}")))?;

        stream
            .shutdown()
            .await
            .map_err(|e| RustboxError::VmBackend(format!("shutdown write: {e}")))?;

        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .map_err(|e| RustboxError::VmBackend(format!("read response: {e}")))?;

        let response_str = String::from_utf8_lossy(&response);

        // Parse HTTP response: status line then headers then body
        let (head, body) = response_str
            .split_once("\r\n\r\n")
            .unwrap_or((&response_str, ""));

        let status_line = head
            .lines()
            .next()
            .ok_or_else(|| RustboxError::VmBackend("empty response".to_string()))?;

        // Status line: "HTTP/1.1 200 OK"
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                RustboxError::VmBackend(format!("invalid status line: {status_line}"))
            })?;

        debug!(
            method = method,
            path = path,
            status = status_code,
            "firecracker API response"
        );

        if !(200..300).contains(&status_code) {
            warn!(
                status = status_code,
                body = body,
                "firecracker API error response"
            );
            return Err(RustboxError::VmBackend(format!(
                "Firecracker API {method} {path} returned {status_code}: {body}"
            )));
        }

        Ok((status_code, body.to_string()))
    }

    /// Helper for PUT requests with a JSON body.
    async fn put(&self, path: &str, body: &serde_json::Value) -> Result<()> {
        let body_str = serde_json::to_string(body)?;
        self.request("PUT", path, Some(&body_str)).await?;
        Ok(())
    }

    /// Helper for PATCH requests with a JSON body.
    async fn patch(&self, path: &str, body: &serde_json::Value) -> Result<()> {
        let body_str = serde_json::to_string(body)?;
        self.request("PATCH", path, Some(&body_str)).await?;
        Ok(())
    }

    pub async fn put_boot_source(
        &self,
        kernel_image_path: &str,
        boot_args: &str,
    ) -> Result<()> {
        self.put(
            "/boot-source",
            &json!({
                "kernel_image_path": kernel_image_path,
                "boot_args": boot_args,
            }),
        )
        .await
    }

    pub async fn put_drive(
        &self,
        drive_id: &str,
        path_on_host: &str,
        is_root: bool,
        is_read_only: bool,
    ) -> Result<()> {
        self.put(
            &format!("/drives/{drive_id}"),
            &json!({
                "drive_id": drive_id,
                "path_on_host": path_on_host,
                "is_root_device": is_root,
                "is_read_only": is_read_only,
            }),
        )
        .await
    }

    pub async fn put_machine_config(
        &self,
        vcpu_count: u8,
        mem_size_mib: u32,
    ) -> Result<()> {
        self.put(
            "/machine-config",
            &json!({
                "vcpu_count": vcpu_count,
                "mem_size_mib": mem_size_mib,
            }),
        )
        .await
    }

    pub async fn put_network_interface(
        &self,
        iface_id: &str,
        tap_name: &str,
        mac: &str,
    ) -> Result<()> {
        self.put(
            &format!("/network-interfaces/{iface_id}"),
            &json!({
                "iface_id": iface_id,
                "host_dev_name": tap_name,
                "guest_mac": mac,
            }),
        )
        .await
    }

    pub async fn put_vsock(
        &self,
        vsock_path: &str,
        guest_cid: u32,
    ) -> Result<()> {
        self.put(
            "/vsock",
            &json!({
                "guest_cid": guest_cid,
                "uds_path": vsock_path,
            }),
        )
        .await
    }

    pub async fn start_instance(&self) -> Result<()> {
        self.put(
            "/actions",
            &json!({
                "action_type": "InstanceStart",
            }),
        )
        .await
    }

    pub async fn stop_instance(&self) -> Result<()> {
        self.put(
            "/actions",
            &json!({
                "action_type": "SendCtrlAltDel",
            }),
        )
        .await
    }

    pub async fn pause_instance(&self) -> Result<()> {
        self.patch(
            "/vm",
            &json!({
                "state": "Paused",
            }),
        )
        .await
    }

    pub async fn create_snapshot(
        &self,
        snapshot_path: &str,
        mem_path: &str,
    ) -> Result<()> {
        self.put(
            "/snapshot/create",
            &json!({
                "snapshot_type": "Full",
                "snapshot_path": snapshot_path,
                "mem_file_path": mem_path,
            }),
        )
        .await
    }

    pub async fn load_snapshot(
        &self,
        snapshot_path: &str,
        mem_path: &str,
    ) -> Result<()> {
        self.put(
            "/snapshot/load",
            &json!({
                "snapshot_path": snapshot_path,
                "mem_file_path": mem_path,
            }),
        )
        .await
    }
}
