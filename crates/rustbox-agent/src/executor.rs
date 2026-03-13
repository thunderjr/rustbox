//! Command execution logic.
//!
//! Manages spawning child processes, streaming their output, and tracking them
//! so they can be killed on request.

use crate::protocol::{AgentResponse, OutputStream};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// Manages running processes inside the guest VM.
pub struct CommandExecutor {
    /// Tracked child processes keyed by command_id.
    children: Arc<Mutex<HashMap<String, Child>>>,
}

impl CommandExecutor {
    pub fn new() -> Self {
        Self {
            children: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Spawn a command and stream output back through the provided sender.
    ///
    /// Returns `(command_id, JoinHandle)`. The join handle completes once the
    /// process exits and all output has been sent.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_command(
        &self,
        cmd: String,
        args: Vec<String>,
        cwd: Option<String>,
        env: Option<HashMap<String, String>>,
        sudo: bool,
        detached: bool,
        response_tx: tokio::sync::mpsc::Sender<AgentResponse>,
    ) -> Result<(String, tokio::task::JoinHandle<()>), String> {
        let command_id = uuid::Uuid::now_v7().to_string();

        let (program, full_args) = if sudo {
            ("sudo".to_string(), {
                let mut a = vec![cmd];
                a.extend(args);
                a
            })
        } else {
            (cmd, args)
        };

        let mut command = Command::new(&program);
        command.args(&full_args);

        if let Some(ref dir) = cwd {
            command.current_dir(dir);
        }
        if let Some(ref envs) = env {
            command.envs(envs);
        }

        // Always capture stdout/stderr for non-detached commands.
        if !detached {
            command.stdout(std::process::Stdio::piped());
            command.stderr(std::process::Stdio::piped());
        } else {
            command.stdout(std::process::Stdio::null());
            command.stderr(std::process::Stdio::null());
        }

        // Prevent the child from inheriting stdin.
        command.stdin(std::process::Stdio::null());

        let mut child = command.spawn().map_err(|e| e.to_string())?;

        info!(command_id = %command_id, program = %program, "spawned process");

        if detached {
            // For detached commands we do not track or stream output.
            let cid = command_id.clone();
            let handle = tokio::spawn(async move {
                let _ = response_tx
                    .send(AgentResponse::ExecStarted {
                        command_id: cid.clone(),
                    })
                    .await;
            });
            return Ok((command_id, handle));
        }

        // Take ownership of stdout/stderr handles before moving child into the map.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        // Track the child so it can be killed later.
        {
            let mut children = self.children.lock().await;
            children.insert(command_id.clone(), child);
        }

        let cid = command_id.clone();
        let children = Arc::clone(&self.children);

        let handle = tokio::spawn(async move {
            // Notify that the process has started.
            let _ = response_tx
                .send(AgentResponse::ExecStarted {
                    command_id: cid.clone(),
                })
                .await;

            // Stream stdout and stderr concurrently.
            let stdout_tx = response_tx.clone();
            let stderr_tx = response_tx.clone();
            let cid_out = cid.clone();
            let cid_err = cid.clone();

            let stdout_task = tokio::spawn(async move {
                if let Some(mut out) = stdout {
                    let mut buf = vec![0u8; 4096];
                    loop {
                        match out.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                let _ = stdout_tx
                                    .send(AgentResponse::Output {
                                        command_id: cid_out.clone(),
                                        stream: OutputStream::Stdout,
                                        data: buf[..n].to_vec(),
                                    })
                                    .await;
                            }
                            Err(e) => {
                                warn!("stdout read error: {e}");
                                break;
                            }
                        }
                    }
                }
            });

            let stderr_task = tokio::spawn(async move {
                if let Some(mut err) = stderr {
                    let mut buf = vec![0u8; 4096];
                    loop {
                        match err.read(&mut buf).await {
                            Ok(0) => break,
                            Ok(n) => {
                                let _ = stderr_tx
                                    .send(AgentResponse::Output {
                                        command_id: cid_err.clone(),
                                        stream: OutputStream::Stderr,
                                        data: buf[..n].to_vec(),
                                    })
                                    .await;
                            }
                            Err(e) => {
                                warn!("stderr read error: {e}");
                                break;
                            }
                        }
                    }
                }
            });

            // Wait for both streams to finish.
            let _ = tokio::join!(stdout_task, stderr_task);

            // Wait for the process to exit and collect the status.
            let exit_code = {
                let mut children = children.lock().await;
                if let Some(mut child) = children.remove(&cid) {
                    match child.wait().await {
                        Ok(status) => status.code().unwrap_or(-1),
                        Err(e) => {
                            error!("wait error: {e}");
                            -1
                        }
                    }
                } else {
                    // Already removed (killed).
                    -1
                }
            };

            let _ = response_tx
                .send(AgentResponse::ExecDone {
                    command_id: cid,
                    exit_code,
                })
                .await;
        });

        Ok((command_id, handle))
    }

    /// Send a signal to a tracked process.
    pub async fn kill_command(&self, command_id: &str, signal: i32) -> Result<(), String> {
        let mut children = self.children.lock().await;
        let child = children
            .get_mut(command_id)
            .ok_or_else(|| format!("no such command: {command_id}"))?;

        let pid = child
            .id()
            .ok_or_else(|| "process already exited".to_string())?;

        // Use nix to send an arbitrary signal.
        let pid = nix::unistd::Pid::from_raw(pid as i32);
        let sig = nix::sys::signal::Signal::try_from(signal)
            .map_err(|e| format!("invalid signal {signal}: {e}"))?;
        nix::sys::signal::kill(pid, sig).map_err(|e| format!("kill failed: {e}"))?;

        info!(command_id = %command_id, signal = signal, "sent signal to process");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// Helper: spawn a command and collect all responses from the channel.
    async fn spawn_and_collect(
        cmd: &str,
        args: Vec<&str>,
    ) -> Result<Vec<AgentResponse>, String> {
        let executor = CommandExecutor::new();
        let (tx, mut rx) = mpsc::channel(64);

        let (_id, handle) = executor
            .spawn_command(
                cmd.into(),
                args.into_iter().map(String::from).collect(),
                None,
                None,
                false,
                false,
                tx,
            )
            .await?;

        handle.await.unwrap();

        let mut responses = Vec::new();
        while let Ok(resp) = rx.try_recv() {
            responses.push(resp);
        }
        Ok(responses)
    }

    #[tokio::test]
    async fn spawn_echo_test() {
        let responses = spawn_and_collect("echo", vec!["test"]).await.unwrap();

        assert!(responses.len() >= 3, "got {} responses: {responses:?}", responses.len());

        assert!(matches!(&responses[0], AgentResponse::ExecStarted { .. }));

        let stdout_data: Vec<u8> = responses
            .iter()
            .filter_map(|r| match r {
                AgentResponse::Output {
                    stream: OutputStream::Stdout,
                    data,
                    ..
                } => Some(data.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(
            String::from_utf8_lossy(&stdout_data),
            "test\n",
            "stdout should contain 'test\\n'"
        );

        let last = responses.last().unwrap();
        assert!(
            matches!(last, AgentResponse::ExecDone { exit_code: 0, .. }),
            "expected ExecDone with exit_code 0, got: {last:?}"
        );
    }

    #[tokio::test]
    async fn spawn_false_exit_code_1() {
        let responses = spawn_and_collect("false", vec![]).await.unwrap();

        let last = responses.last().unwrap();
        match last {
            AgentResponse::ExecDone { exit_code, .. } => {
                assert_ne!(*exit_code, 0, "false should exit with non-zero code");
            }
            other => panic!("expected ExecDone, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn spawn_nonexistent_binary() {
        let result = spawn_and_collect("nonexistent_binary_xyz_12345", vec![]).await;
        assert!(result.is_err(), "spawning nonexistent binary should fail");
    }
}
