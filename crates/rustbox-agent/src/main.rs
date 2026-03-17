//! rustbox-agent -- lightweight agent that runs inside the guest VM.
//!
//! Listens for length-prefixed JSON requests on a socket and executes them.
//!
//! Transport selection:
//! - If `/dev/vsock` exists or `RUSTBOX_TRANSPORT=vsock` is set: listen on vsock port 5123
//! - Otherwise: listen on TCP 0.0.0.0:5123

mod executor;
mod handler;
mod protocol;
mod transport;

use executor::CommandExecutor;
use protocol::AgentResponse;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Port the agent listens on (both TCP and vsock).
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

    let use_vsock = std::env::var("RUSTBOX_TRANSPORT")
        .map(|v| v == "vsock")
        .unwrap_or(false)
        || std::path::Path::new("/dev/vsock").exists();

    if use_vsock {
        #[cfg(target_os = "linux")]
        {
            listen_vsock(executor).await?;
        }
        #[cfg(not(target_os = "linux"))]
        {
            anyhow::bail!("vsock transport is only available on Linux");
        }
    } else {
        listen_tcp(executor).await?;
    }

    Ok(())
}

async fn listen_tcp(executor: Arc<CommandExecutor>) -> anyhow::Result<()> {
    let addr = format!("0.0.0.0:{AGENT_PORT}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(addr = %addr, "agent listening on TCP");

    loop {
        let (stream, peer) = listener.accept().await?;
        info!(peer = %peer, "accepted TCP connection");

        let executor = Arc::clone(&executor);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, executor).await {
                warn!(peer = %peer, error = %e, "connection error");
            }
            info!(peer = %peer, "connection closed");
        });
    }
}

#[cfg(target_os = "linux")]
async fn listen_vsock(executor: Arc<CommandExecutor>) -> anyhow::Result<()> {
    use tokio_vsock::VsockListener;

    let listener = VsockListener::bind(libc::VMADDR_CID_ANY, AGENT_PORT as u32)?;
    info!(port = AGENT_PORT, "agent listening on vsock");

    loop {
        let (stream, peer) = listener.accept().await?;
        info!(peer = ?peer, "accepted vsock connection");

        let executor = Arc::clone(&executor);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, executor).await {
                warn!(peer = ?peer, error = %e, "connection error");
            }
            info!(peer = ?peer, "connection closed");
        });
    }
}

/// Handle a single client connection: read requests, process them, write
/// responses. Each request may produce multiple responses (e.g. Exec streams
/// output), so we use an mpsc channel internally.
async fn handle_connection<S>(
    stream: S,
    executor: Arc<CommandExecutor>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (reader, writer) = tokio::io::split(stream);
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
