use rustbox_core::protocol::{AgentRequest, AgentResponse};
use rustbox_core::{CommandId, CommandOutput, Result, RustboxError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::debug;

/// Maximum message size (16 MiB).
const MAX_MESSAGE_SIZE: u32 = 16 * 1024 * 1024;

/// Client for communicating with the guest agent.
pub struct AgentClient {
    host: String,
    port: u16,
}

impl AgentClient {
    /// Create a new agent client that connects via TCP.
    pub fn new_tcp(host: String, port: u16) -> Self {
        Self { host, port }
    }

    /// Establish a connection to the guest agent.
    pub async fn connect(&self) -> Result<AgentConnection> {
        let addr = format!("{}:{}", self.host, self.port);
        debug!(addr = %addr, "connecting to guest agent");

        let stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| RustboxError::AgentComm(format!("connect to agent at {addr}: {e}")))?;

        Ok(AgentConnection { stream })
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
/// JSON framing over a TCP stream.
pub struct AgentConnection {
    stream: TcpStream,
}

impl AgentConnection {
    /// Send a length-prefixed JSON request.
    pub async fn send_request(&mut self, req: &AgentRequest) -> Result<()> {
        let payload = serde_json::to_vec(req)?;
        let len = payload.len() as u32;
        self.stream
            .write_all(&len.to_be_bytes())
            .await
            .map_err(|e| RustboxError::AgentComm(format!("write length: {e}")))?;
        self.stream
            .write_all(&payload)
            .await
            .map_err(|e| RustboxError::AgentComm(format!("write payload: {e}")))?;
        self.stream
            .flush()
            .await
            .map_err(|e| RustboxError::AgentComm(format!("flush: {e}")))?;
        Ok(())
    }

    /// Read a length-prefixed JSON response.
    pub async fn recv_response(&mut self) -> Result<AgentResponse> {
        let mut len_buf = [0u8; 4];
        self.stream
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
        self.stream
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
}
