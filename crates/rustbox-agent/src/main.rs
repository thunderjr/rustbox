//! rustbox-agent -- lightweight agent that runs inside the guest VM.
//!
//! Listens for length-prefixed JSON requests on a socket and executes them.
//!
//! Phase 1: listens on TCP 127.0.0.1:5123 for easy local testing.
//! Phase 2: switch to virtio-vsock (CID=3, port 5123) using tokio-vsock.

mod executor;
mod handler;
mod protocol;
mod transport;

use executor::CommandExecutor;
use protocol::AgentResponse;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// The vsock CID for the guest (reserved, will be used in Phase 2).
const _VSOCK_CID: u32 = 3;

/// Port the agent listens on.
const AGENT_PORT: u16 = 5123;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialise tracing.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let executor = Arc::new(CommandExecutor::new());

    // TODO(phase2): Replace TCP with vsock listener:
    //   let listener = VsockListener::bind(VSOCK_CID, AGENT_PORT)?;
    let addr = format!("0.0.0.0:{AGENT_PORT}");
    let listener = TcpListener::bind(&addr).await?;
    info!(addr = %addr, "agent listening");

    loop {
        let (stream, peer) = listener.accept().await?;
        info!(peer = %peer, "accepted connection");

        let executor = Arc::clone(&executor);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, executor).await {
                warn!(peer = %peer, error = %e, "connection error");
            }
            info!(peer = %peer, "connection closed");
        });
    }
}

/// Handle a single client connection: read requests, process them, write
/// responses. Each request may produce multiple responses (e.g. Exec streams
/// output), so we use an mpsc channel internally.
async fn handle_connection(
    stream: tokio::net::TcpStream,
    executor: Arc<CommandExecutor>,
) -> anyhow::Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);

    loop {
        // Read the next request.
        let request = match transport::read_message(&mut reader).await {
            Ok(req) => req,
            Err(transport::TransportError::ConnectionClosed) => return Ok(()),
            Err(e) => return Err(e.into()),
        };

        // Create a channel for responses from the handler.
        let (tx, mut rx) = mpsc::channel::<AgentResponse>(64);

        // Spawn the handler so it can run concurrently (especially for Exec
        // which streams output over time).
        let exec = Arc::clone(&executor);
        let handler_task = tokio::spawn(async move {
            handler::handle_request(request, exec, tx).await;
        });

        // Forward every response from the handler to the wire.
        while let Some(response) = rx.recv().await {
            if let Err(e) = transport::write_message(&mut writer, &response).await {
                error!("write error: {e}");
                return Err(e.into());
            }
        }

        // Flush after all responses for this request have been sent.
        writer.flush().await?;

        // Ensure the handler task finished cleanly.
        let _ = handler_task.await;
    }
}
