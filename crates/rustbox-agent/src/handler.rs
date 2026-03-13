//! Request handler.
//!
//! Routes each `AgentRequest` to the appropriate logic and streams back one or
//! more `AgentResponse` messages through the provided sender.

use crate::executor::CommandExecutor;
use crate::protocol::{AgentRequest, AgentResponse};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};

/// Handles a single request, sending responses through `tx`.
///
/// Some requests (like `Exec`) produce multiple responses (started, output
/// chunks, done), so we use a channel rather than returning a single value.
pub async fn handle_request(
    request: AgentRequest,
    executor: Arc<CommandExecutor>,
    tx: mpsc::Sender<AgentResponse>,
) {
    match request {
        AgentRequest::Ping => {
            info!("ping");
            let _ = tx.send(AgentResponse::Pong).await;
        }

        AgentRequest::Exec {
            cmd,
            args,
            cwd,
            env,
            sudo,
            detached,
        } => {
            info!(cmd = %cmd, "exec");
            match executor
                .spawn_command(cmd, args, cwd, env, sudo, detached, tx.clone())
                .await
            {
                Ok((_command_id, handle)) => {
                    // Wait for the spawned task to finish streaming output.
                    let _ = handle.await;
                }
                Err(e) => {
                    error!("spawn failed: {e}");
                    let _ = tx
                        .send(AgentResponse::Error {
                            message: format!("failed to spawn command: {e}"),
                        })
                        .await;
                }
            }
        }

        AgentRequest::Kill {
            command_id,
            signal,
        } => {
            info!(command_id = %command_id, signal = signal, "kill");
            match executor.kill_command(&command_id, signal).await {
                Ok(()) => {
                    let _ = tx.send(AgentResponse::Ok).await;
                }
                Err(e) => {
                    let _ = tx
                        .send(AgentResponse::Error { message: e })
                        .await;
                }
            }
        }

        AgentRequest::WriteFile { path, content } => {
            info!(path = %path, "write_file");
            let result = async {
                // Create parent directories if needed.
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(&path, &content).await
            }
            .await;

            match result {
                Ok(()) => {
                    let _ = tx.send(AgentResponse::Ok).await;
                }
                Err(e) => {
                    let _ = tx
                        .send(AgentResponse::Error {
                            message: format!("write_file failed: {e}"),
                        })
                        .await;
                }
            }
        }

        AgentRequest::ReadFile { path } => {
            info!(path = %path, "read_file");
            match tokio::fs::read(&path).await {
                Ok(data) => {
                    let _ = tx.send(AgentResponse::FileContent { data }).await;
                }
                Err(e) => {
                    let _ = tx
                        .send(AgentResponse::Error {
                            message: format!("read_file failed: {e}"),
                        })
                        .await;
                }
            }
        }

        AgentRequest::Mkdir { path } => {
            info!(path = %path, "mkdir");
            match tokio::fs::create_dir_all(&path).await {
                Ok(()) => {
                    let _ = tx.send(AgentResponse::Ok).await;
                }
                Err(e) => {
                    let _ = tx
                        .send(AgentResponse::Error {
                            message: format!("mkdir failed: {e}"),
                        })
                        .await;
                }
            }
        }

        AgentRequest::Metrics => {
            info!("metrics");
            // Stub: return zeros for now. In a real guest we would read from
            // /proc/stat, /proc/meminfo, /proc/net/dev, /proc/diskstats, etc.
            let _ = tx
                .send(AgentResponse::MetricsResult {
                    cpu_usage_percent: 0.0,
                    memory_used_bytes: 0,
                    memory_total_bytes: 0,
                    network_rx_bytes: 0,
                    network_tx_bytes: 0,
                    disk_used_bytes: 0,
                })
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: send a request through handle_request and collect all responses.
    async fn send_and_collect(request: AgentRequest) -> Vec<AgentResponse> {
        let executor = Arc::new(CommandExecutor::new());
        let (tx, mut rx) = mpsc::channel(64);

        handle_request(request, executor, tx).await;

        let mut responses = Vec::new();
        while let Ok(resp) = rx.try_recv() {
            responses.push(resp);
        }
        responses
    }

    #[tokio::test]
    async fn ping_pong() {
        let responses = send_and_collect(AgentRequest::Ping).await;
        assert_eq!(responses.len(), 1);
        assert!(matches!(responses[0], AgentResponse::Pong));
    }

    #[tokio::test]
    async fn exec_echo_hello() {
        let responses = send_and_collect(AgentRequest::Exec {
            cmd: "echo".into(),
            args: vec!["hello".into()],
            cwd: None,
            env: None,
            sudo: false,
            detached: false,
        })
        .await;

        // Should have ExecStarted, at least one Output, and ExecDone.
        assert!(responses.len() >= 3, "got {} responses: {responses:?}", responses.len());

        assert!(
            matches!(&responses[0], AgentResponse::ExecStarted { .. }),
            "first response should be ExecStarted, got: {:?}",
            responses[0]
        );

        // Collect all stdout data.
        let stdout_data: Vec<u8> = responses
            .iter()
            .filter_map(|r| match r {
                AgentResponse::Output {
                    stream: crate::protocol::OutputStream::Stdout,
                    data,
                    ..
                } => Some(data.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(String::from_utf8_lossy(&stdout_data), "hello\n");

        let last = responses.last().unwrap();
        assert!(
            matches!(last, AgentResponse::ExecDone { exit_code: 0, .. }),
            "last response should be ExecDone with exit_code 0, got: {last:?}"
        );
    }

    #[tokio::test]
    async fn write_and_read_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("test.txt");
        let file_path_str = file_path.to_str().unwrap().to_string();
        let content = b"hello world".to_vec();

        // Write.
        let write_responses = send_and_collect(AgentRequest::WriteFile {
            path: file_path_str.clone(),
            content: content.clone(),
        })
        .await;
        assert_eq!(write_responses.len(), 1);
        assert!(matches!(write_responses[0], AgentResponse::Ok));

        // Read back.
        let read_responses = send_and_collect(AgentRequest::ReadFile {
            path: file_path_str,
        })
        .await;
        assert_eq!(read_responses.len(), 1);
        match &read_responses[0] {
            AgentResponse::FileContent { data } => {
                assert_eq!(data, &content);
            }
            other => panic!("expected FileContent, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn mkdir_creates_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let dir_path = tmp.path().join("sub/nested");
        let dir_path_str = dir_path.to_str().unwrap().to_string();

        let responses = send_and_collect(AgentRequest::Mkdir {
            path: dir_path_str.clone(),
        })
        .await;
        assert_eq!(responses.len(), 1);
        assert!(matches!(responses[0], AgentResponse::Ok));
        assert!(dir_path.is_dir(), "directory should exist after Mkdir");
    }
}
