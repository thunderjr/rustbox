use std::path::PathBuf;

use rustbox_core::protocol::{AgentRequest, AgentResponse};
use rustbox_core::{CommandId, CommandOutput, Result, RustboxError};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use tracing::debug;

/// Maximum message size (16 MiB).
const MAX_MESSAGE_SIZE: u32 = 16 * 1024 * 1024;

/// Transport configuration for agent connections.
#[derive(Clone, Debug)]
pub enum AgentTransport {
    Tcp { host: String, port: u16 },
    Vsock { uds_path: PathBuf, guest_port: u32 },
}

/// Client for communicating with the guest agent.
pub struct AgentClient {
    transport: AgentTransport,
}

impl AgentClient {
    /// Create a new agent client that connects via TCP.
    pub fn new_tcp(host: String, port: u16) -> Self {
        Self {
            transport: AgentTransport::Tcp { host, port },
        }
    }

    /// Create a new agent client that connects via vsock (Firecracker UDS).
    pub fn new_vsock(uds_path: PathBuf, guest_port: u32) -> Self {
        Self {
            transport: AgentTransport::Vsock { uds_path, guest_port },
        }
    }

    /// Establish a connection to the guest agent.
    pub async fn connect(&self) -> Result<AgentConnection> {
        match &self.transport {
            AgentTransport::Tcp { host, port } => {
                let addr = format!("{host}:{port}");
                debug!(addr = %addr, "connecting to guest agent via TCP");

                let stream = tokio::net::TcpStream::connect(&addr)
                    .await
                    .map_err(|e| RustboxError::AgentComm(format!("connect to agent at {addr}: {e}")))?;

                let (reader, writer) = tokio::io::split(stream);
                Ok(AgentConnection {
                    reader: Box::new(reader),
                    writer: Box::new(writer),
                })
            }
            AgentTransport::Vsock { uds_path, guest_port } => {
                debug!(path = %uds_path.display(), port = guest_port, "connecting to guest agent via vsock");

                let stream = tokio::net::UnixStream::connect(uds_path)
                    .await
                    .map_err(|e| RustboxError::AgentComm(format!(
                        "connect to vsock at {}: {e}", uds_path.display()
                    )))?;

                let (mut reader, mut writer) = tokio::io::split(stream);

                // Firecracker vsock multiplexer handshake: send CONNECT <port>\n
                let connect_msg = format!("CONNECT {guest_port}\n");
                writer.write_all(connect_msg.as_bytes()).await.map_err(|e| {
                    RustboxError::AgentComm(format!("vsock handshake write: {e}"))
                })?;
                writer.flush().await.map_err(|e| {
                    RustboxError::AgentComm(format!("vsock handshake flush: {e}"))
                })?;

                // Read "OK <port>\n" response
                let mut buf_reader = BufReader::new(&mut reader);
                let mut line = String::new();
                buf_reader.read_line(&mut line).await.map_err(|e| {
                    RustboxError::AgentComm(format!("vsock handshake read: {e}"))
                })?;

                if !line.starts_with("OK") {
                    return Err(RustboxError::AgentComm(format!(
                        "vsock handshake failed: expected 'OK ...', got '{}'",
                        line.trim()
                    )));
                }

                debug!("vsock handshake complete");
                Ok(AgentConnection {
                    reader: Box::new(reader),
                    writer: Box::new(writer),
                })
            }
        }
    }

    /// Execute a command and stream output back over the channel.
    pub async fn exec_streaming(
        &self,
        request: AgentRequest,
        cmd_id: CommandId,
        tx: mpsc::Sender<CommandOutput>,
    ) -> Result<()> {
        let mut conn = self.connect().await?;
        conn.send_request(&request).await?;

        // Read streaming responses until we get ExecDone or an error.
        loop {
            let resp = conn.recv_response().await?;
            match resp {
                AgentResponse::ExecStarted { .. } => {
                    debug!(cmd_id = %cmd_id, "exec started");
                }
                AgentResponse::Output {
                    stream, data, ..
                } => {
                    let output = match stream {
                        rustbox_core::protocol::OutputStream::Stdout => {
                            CommandOutput::Stdout(data)
                        }
                        rustbox_core::protocol::OutputStream::Stderr => {
                            CommandOutput::Stderr(data)
                        }
                    };
                    if tx.send(output).await.is_err() {
                        // Receiver dropped, stop reading.
                        break;
                    }
                }
                AgentResponse::ExecDone { exit_code, .. } => {
                    let _ = tx.send(CommandOutput::Exit(exit_code)).await;
                    break;
                }
                AgentResponse::Error { message } => {
                    return Err(RustboxError::AgentComm(message));
                }
                _ => {
                    return Err(RustboxError::AgentComm(
                        "unexpected response during exec".to_string(),
                    ));
                }
            }
        }

        Ok(())
    }
}

/// An established connection to the guest agent, providing length-prefixed
/// JSON framing over any async stream (TCP, Unix socket via vsock, etc.).
pub struct AgentConnection {
    reader: Box<dyn AsyncRead + Unpin + Send>,
    writer: Box<dyn AsyncWrite + Unpin + Send>,
}

impl AgentConnection {
    /// Send a length-prefixed JSON request.
    pub async fn send_request(&mut self, req: &AgentRequest) -> Result<()> {
        let payload = serde_json::to_vec(req)?;
        let len = payload.len() as u32;
        self.writer
            .write_all(&len.to_be_bytes())
            .await
            .map_err(|e| RustboxError::AgentComm(format!("write length: {e}")))?;
        self.writer
            .write_all(&payload)
            .await
            .map_err(|e| RustboxError::AgentComm(format!("write payload: {e}")))?;
        self.writer
            .flush()
            .await
            .map_err(|e| RustboxError::AgentComm(format!("flush: {e}")))?;
        Ok(())
    }

    /// Read a length-prefixed JSON response.
    pub async fn recv_response(&mut self) -> Result<AgentResponse> {
        let mut len_buf = [0u8; 4];
        self.reader
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| RustboxError::AgentComm(format!("read length: {e}")))?;

        let len = u32::from_be_bytes(len_buf);
        if len > MAX_MESSAGE_SIZE {
            return Err(RustboxError::AgentComm(format!(
                "message too large: {len} bytes"
            )));
        }

        let mut buf = vec![0u8; len as usize];
        self.reader
            .read_exact(&mut buf)
            .await
            .map_err(|e| RustboxError::AgentComm(format!("read payload: {e}")))?;

        let response: AgentResponse = serde_json::from_slice(&buf)?;
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustbox_core::protocol::{AgentRequest, AgentResponse};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Helper: start a TCP server that reads one length-prefixed JSON request,
    /// deserializes it, and responds with the given `AgentResponse`.
    async fn spawn_test_server(
        response: AgentResponse,
    ) -> (u16, tokio::task::JoinHandle<AgentRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            // Read length-prefixed request
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await.unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut buf = vec![0u8; len];
            stream.read_exact(&mut buf).await.unwrap();
            let request: AgentRequest = serde_json::from_slice(&buf).unwrap();

            // Write length-prefixed response
            let resp_bytes = serde_json::to_vec(&response).unwrap();
            let resp_len = (resp_bytes.len() as u32).to_be_bytes();
            stream.write_all(&resp_len).await.unwrap();
            stream.write_all(&resp_bytes).await.unwrap();
            stream.flush().await.unwrap();

            request
        });

        (port, handle)
    }

    #[tokio::test]
    async fn ping_pong_roundtrip() {
        let (port, server_handle) = spawn_test_server(AgentResponse::Pong).await;

        let client = AgentClient::new_tcp("127.0.0.1".into(), port);
        let mut conn = client.connect().await.unwrap();

        conn.send_request(&AgentRequest::Ping).await.unwrap();
        let response = conn.recv_response().await.unwrap();

        assert!(
            matches!(response, AgentResponse::Pong),
            "expected Pong, got: {response:?}"
        );

        // Verify the server received Ping
        let received = server_handle.await.unwrap();
        assert!(
            matches!(received, AgentRequest::Ping),
            "server should have received Ping, got: {received:?}"
        );
    }

    #[tokio::test]
    async fn length_prefixed_framing_roundtrip() {
        // Server echoes back an Ok response for any request
        let (port, server_handle) = spawn_test_server(AgentResponse::Ok).await;

        let client = AgentClient::new_tcp("127.0.0.1".into(), port);
        let mut conn = client.connect().await.unwrap();

        // Send a Metrics request (arbitrary choice) to verify framing works
        conn.send_request(&AgentRequest::Metrics).await.unwrap();
        let response = conn.recv_response().await.unwrap();

        assert!(
            matches!(response, AgentResponse::Ok),
            "expected Ok, got: {response:?}"
        );

        let received = server_handle.await.unwrap();
        assert!(
            matches!(received, AgentRequest::Metrics),
            "server should have received Metrics, got: {received:?}"
        );
    }

    #[tokio::test]
    async fn oversized_message_rejection() {
        // Server sends a length prefix exceeding MAX_MESSAGE_SIZE
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();

            // Read the client's request (we don't care about it)
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).await.unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut buf = vec![0u8; len];
            stream.read_exact(&mut buf).await.unwrap();

            // Send back an oversized length prefix (MAX_MESSAGE_SIZE + 1)
            let bad_len: u32 = MAX_MESSAGE_SIZE + 1;
            stream.write_all(&bad_len.to_be_bytes()).await.unwrap();
            stream.flush().await.unwrap();
        });

        let client = AgentClient::new_tcp("127.0.0.1".into(), port);
        let mut conn = client.connect().await.unwrap();

        conn.send_request(&AgentRequest::Ping).await.unwrap();
        let result = conn.recv_response().await;

        assert!(result.is_err(), "should reject oversized message");
        let err_msg = format!("{}", result.unwrap_err());
        assert!(
            err_msg.contains("message too large"),
            "error should mention size: {err_msg}"
        );
    }

    #[tokio::test]
    async fn vsock_handshake_protocol() {
        // Simulate the vsock multiplexer handshake using a duplex stream
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);

        // Server side: expect "CONNECT 5123\n", respond "OK 5123\n", then do
        // a length-prefixed Pong exchange
        let server_handle = tokio::spawn(async move {
            let mut buf = vec![0u8; 64];
            let mut read_so_far = 0;

            // Read until we get the newline
            loop {
                let n = server_stream.read(&mut buf[read_so_far..]).await.unwrap();
                read_so_far += n;
                if buf[..read_so_far].contains(&b'\n') {
                    break;
                }
            }

            let connect_msg = String::from_utf8_lossy(&buf[..read_so_far]);
            assert!(
                connect_msg.starts_with("CONNECT 5123"),
                "expected CONNECT message, got: {connect_msg}"
            );

            // Respond with OK
            server_stream.write_all(b"OK 5123\n").await.unwrap();
            server_stream.flush().await.unwrap();

            // Now read a length-prefixed request
            let mut len_buf = [0u8; 4];
            server_stream.read_exact(&mut len_buf).await.unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut payload = vec![0u8; len];
            server_stream.read_exact(&mut payload).await.unwrap();
            let req: AgentRequest = serde_json::from_slice(&payload).unwrap();
            assert!(matches!(req, AgentRequest::Ping));

            // Send back a Pong
            let resp = serde_json::to_vec(&AgentResponse::Pong).unwrap();
            let resp_len = (resp.len() as u32).to_be_bytes();
            server_stream.write_all(&resp_len).await.unwrap();
            server_stream.write_all(&resp).await.unwrap();
            server_stream.flush().await.unwrap();
        });

        // Client side: use a UnixStream-like path but we'll test the handshake
        // logic directly by constructing the connection manually.
        // Since we can't easily use new_vsock with a duplex, we test the
        // handshake logic by simulating what connect() does.
        let (reader, mut writer) = tokio::io::split(client_stream);

        // Send CONNECT
        writer.write_all(b"CONNECT 5123\n").await.unwrap();
        writer.flush().await.unwrap();

        // Read OK response
        let mut buf_reader = BufReader::new(reader);
        let mut line = String::new();
        buf_reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("OK"), "expected OK, got: {line}");

        // Now do a normal length-prefixed exchange
        let reader = buf_reader.into_inner();
        let mut conn = AgentConnection {
            reader: Box::new(reader),
            writer: Box::new(writer),
        };

        conn.send_request(&AgentRequest::Ping).await.unwrap();
        let resp = conn.recv_response().await.unwrap();
        assert!(matches!(resp, AgentResponse::Pong));

        server_handle.await.unwrap();
    }
}
