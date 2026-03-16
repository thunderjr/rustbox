use std::path::{Path, PathBuf};

use rustbox_core::{Result, RustboxError};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixStream};
use tracing::{debug, warn};

/// Transport type for connecting to the Firecracker API.
pub enum FirecrackerTransport {
    /// Connect via Unix domain socket (used on Linux with local Firecracker).
    Unix(PathBuf),
    /// Connect via TCP (used when forwarding through Lima SSH tunnel).
    Tcp(String, u16),
}

/// Client for the Firecracker REST API over a Unix domain socket or TCP connection.
pub struct FirecrackerClient {
    transport: FirecrackerTransport,
}

impl FirecrackerClient {
    pub fn new(socket_path: &Path) -> Self {
        Self {
            transport: FirecrackerTransport::Unix(socket_path.to_path_buf()),
        }
    }

    pub fn new_tcp(host: &str, port: u16) -> Self {
        Self {
            transport: FirecrackerTransport::Tcp(host.to_string(), port),
        }
    }

    /// Send a raw HTTP/1.1 request and return (status_code, body).
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, String)> {
        match &self.transport {
            FirecrackerTransport::Unix(socket_path) => {
                let stream = UnixStream::connect(socket_path)
                    .await
                    .map_err(|e| RustboxError::VmBackend(format!("connect to socket: {e}")))?;
                self.do_request(stream, method, path, body).await
            }
            FirecrackerTransport::Tcp(host, port) => {
                let stream = TcpStream::connect(format!("{host}:{port}"))
                    .await
                    .map_err(|e| RustboxError::VmBackend(format!("connect to tcp: {e}")))?;
                self.do_request(stream, method, path, body).await
            }
        }
    }

    /// Execute the HTTP request over any async stream.
    async fn do_request(
        &self,
        mut stream: impl AsyncRead + AsyncWrite + Unpin,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<(u16, String)> {

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

        // Read the HTTP response by parsing headers then reading Content-Length
        // bytes. We avoid shutdown()+read_to_end() because through an SSH tunnel
        // the half-close can tear down the channel, and without it read_to_end
        // blocks forever waiting for the remote to close.
        let mut reader = BufReader::new(stream);

        // Read status line.
        let mut status_line = String::new();
        reader
            .read_line(&mut status_line)
            .await
            .map_err(|e| RustboxError::VmBackend(format!("read status line: {e}")))?;
        let status_line = status_line.trim_end();

        if status_line.is_empty() {
            return Err(RustboxError::VmBackend("empty response".to_string()));
        }

        // Status line: "HTTP/1.1 200 OK"
        let status_code: u16 = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                RustboxError::VmBackend(format!("invalid status line: {status_line}"))
            })?;

        // Read headers until blank line, extract Content-Length.
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .map_err(|e| RustboxError::VmBackend(format!("read header: {e}")))?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(val) = trimmed.strip_prefix("Content-Length:") {
                if let Ok(len) = val.trim().parse::<usize>() {
                    content_length = len;
                }
            }
            // Also check lowercase (HTTP headers are case-insensitive).
            if let Some(val) = trimmed.strip_prefix("content-length:") {
                if let Ok(len) = val.trim().parse::<usize>() {
                    content_length = len;
                }
            }
        }

        // Read exactly content_length bytes of body.
        let mut body_buf = vec![0u8; content_length];
        if content_length > 0 {
            use tokio::io::AsyncReadExt;
            reader
                .read_exact(&mut body_buf)
                .await
                .map_err(|e| RustboxError::VmBackend(format!("read body: {e}")))?;
        }
        let body = String::from_utf8_lossy(&body_buf).to_string();

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

        Ok((status_code, body))
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
