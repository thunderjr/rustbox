//! Wire protocol helpers for length-prefixed JSON framing.
//!
//! Every message on the wire is:
//!   [4 bytes big-endian length N] [N bytes of JSON]

use crate::protocol::{AgentRequest, AgentResponse};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum message size (16 MiB) to guard against malformed length prefixes.
const MAX_MESSAGE_SIZE: u32 = 16 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("message too large: {0} bytes (max {MAX_MESSAGE_SIZE})")]
    MessageTooLarge(u32),
    #[error("connection closed")]
    ConnectionClosed,
}

pub type Result<T> = std::result::Result<T, TransportError>;

/// Read a single length-prefixed JSON request from the given reader.
pub async fn read_message<R: AsyncRead + Unpin>(reader: &mut R) -> Result<AgentRequest> {
    // Read 4-byte big-endian length prefix.
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(TransportError::ConnectionClosed);
        }
        Err(e) => return Err(TransportError::Io(e)),
    }
    let len = u32::from_be_bytes(len_buf);

    if len > MAX_MESSAGE_SIZE {
        return Err(TransportError::MessageTooLarge(len));
    }

    // Read the JSON payload.
    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;

    let request: AgentRequest = serde_json::from_slice(&buf)?;
    Ok(request)
}

/// Write a single length-prefixed JSON response to the given writer.
pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    msg: &AgentResponse,
) -> Result<()> {
    let payload = serde_json::to_vec(msg)?;
    let len = payload.len() as u32;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    /// Helper: write a length-prefixed JSON payload into a writer.
    async fn write_length_prefixed<W: AsyncWrite + Unpin, T: serde::Serialize>(
        writer: &mut W,
        value: &T,
    ) {
        let payload = serde_json::to_vec(value).unwrap();
        let len = payload.len() as u32;
        writer.write_all(&len.to_be_bytes()).await.unwrap();
        writer.write_all(&payload).await.unwrap();
        writer.flush().await.unwrap();
    }

    #[tokio::test]
    async fn read_write_roundtrip() {
        let (mut client, mut server) = tokio::io::duplex(4096);

        // Write a Ping request into the client side, then read it from server side.
        let request = AgentRequest::Ping;
        write_length_prefixed(&mut client, &request).await;
        drop(client); // close so read_message doesn't hang

        let received = read_message(&mut server).await.unwrap();
        assert!(matches!(received, AgentRequest::Ping));

        // Also verify write_message for responses.
        let (mut client2, mut server2) = tokio::io::duplex(4096);
        let response = AgentResponse::Pong;
        write_message(&mut client2, &response).await.unwrap();
        drop(client2);

        // Manually read length-prefixed JSON from server2.
        let mut len_buf = [0u8; 4];
        tokio::io::AsyncReadExt::read_exact(&mut server2, &mut len_buf)
            .await
            .unwrap();
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        tokio::io::AsyncReadExt::read_exact(&mut server2, &mut buf)
            .await
            .unwrap();
        let decoded: AgentResponse = serde_json::from_slice(&buf).unwrap();
        assert!(matches!(decoded, AgentResponse::Pong));
    }

    #[tokio::test]
    async fn oversized_message_error() {
        let (mut client, mut server) = tokio::io::duplex(1024);

        // Write a length prefix that exceeds MAX_MESSAGE_SIZE.
        let bad_len: u32 = MAX_MESSAGE_SIZE + 1;
        client.write_all(&bad_len.to_be_bytes()).await.unwrap();
        drop(client);

        let err = read_message(&mut server).await.unwrap_err();
        assert!(
            matches!(err, TransportError::MessageTooLarge(n) if n == bad_len),
            "expected MessageTooLarge, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn connection_closed_error() {
        let (client, mut server) = tokio::io::duplex(1024);

        // Immediately drop the writer side.
        drop(client);

        let err = read_message(&mut server).await.unwrap_err();
        assert!(
            matches!(err, TransportError::ConnectionClosed),
            "expected ConnectionClosed, got: {err:?}"
        );
    }
}
