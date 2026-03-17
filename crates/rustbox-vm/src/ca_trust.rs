//! Guest CA trust installation helpers.
//!
//! Writes the proxy CA certificate to the guest filesystem and runs
//! `update-ca-certificates` so the guest trusts HTTPS MITM connections.

use rustbox_core::protocol::{AgentRequest, AgentResponse};
use rustbox_core::{CommandRequest, Result, RustboxError};
use tracing::{debug, warn};

use crate::agent_client::AgentClient;

/// Path inside the guest where the proxy CA certificate is written.
const CA_CERT_GUEST_PATH: &str = "/usr/local/share/ca-certificates/rustbox-proxy.crt";

/// Path for proxy environment config inside the guest.
const PROXY_ENV_PATH: &str = "/etc/profile.d/rustbox-proxy.sh";

/// Install the proxy CA certificate in the guest and update the trust store.
///
/// Writes the PEM-encoded CA cert to the standard Debian/Ubuntu CA directory
/// and runs `update-ca-certificates`. Errors are logged but not propagated,
/// since cert trust failure will surface as HTTPS errors inside the sandbox.
pub async fn install_ca_cert(agent: &AgentClient, cert_pem: &str) {
    // Write the CA certificate.
    if let Err(e) = write_file_via_agent(agent, CA_CERT_GUEST_PATH, cert_pem.as_bytes()).await {
        warn!(error = %e, "failed to write CA cert to guest");
        return;
    }
    debug!("wrote proxy CA cert to guest at {CA_CERT_GUEST_PATH}");

    // Run update-ca-certificates (fire-and-forget — drain until done).
    if let Err(e) = exec_and_wait(agent, "update-ca-certificates", &[]).await {
        warn!(error = %e, "failed to run update-ca-certificates in guest");
    } else {
        debug!("updated guest CA trust store");
    }
}

/// Write proxy environment variables to the guest so tools pick up the proxy.
///
/// This writes to `/etc/profile.d/rustbox-proxy.sh` which is sourced by
/// login shells. For non-shell programs, the CA cert trust is the primary
/// mechanism.
pub async fn write_proxy_env(agent: &AgentClient, proxy_host: &str, proxy_port: u16) {
    let content = format!(
        "export HTTP_PROXY=http://{proxy_host}:{proxy_port}\n\
         export HTTPS_PROXY=http://{proxy_host}:{proxy_port}\n\
         export http_proxy=http://{proxy_host}:{proxy_port}\n\
         export https_proxy=http://{proxy_host}:{proxy_port}\n\
         export NO_PROXY=localhost,127.0.0.1\n\
         export no_proxy=localhost,127.0.0.1\n"
    );

    if let Err(e) = write_file_via_agent(agent, PROXY_ENV_PATH, content.as_bytes()).await {
        warn!(error = %e, "failed to write proxy env to guest");
    } else {
        debug!("wrote proxy env config to guest at {PROXY_ENV_PATH}");
    }
}

/// Remove proxy environment config from the guest.
pub async fn remove_proxy_env(agent: &AgentClient) {
    // Write an empty file to effectively remove the env vars on next shell.
    if let Err(e) = write_file_via_agent(agent, PROXY_ENV_PATH, b"").await {
        warn!(error = %e, "failed to remove proxy env from guest");
    }
}

/// Write a file to the guest via the agent protocol.
async fn write_file_via_agent(agent: &AgentClient, path: &str, content: &[u8]) -> Result<()> {
    let request = AgentRequest::WriteFile {
        path: path.to_string(),
        content: content.to_vec(),
    };
    let mut conn = agent.connect().await?;
    conn.send_request(&request).await?;
    let resp = conn.recv_response().await?;
    match resp {
        AgentResponse::Ok => Ok(()),
        AgentResponse::Error { message } => Err(RustboxError::AgentComm(message)),
        _ => Ok(()), // Accept any non-error response
    }
}

/// Execute a command in the guest and wait for it to complete.
async fn exec_and_wait(agent: &AgentClient, cmd: &str, args: &[&str]) -> Result<()> {
    let request = AgentRequest::Exec(CommandRequest {
        cmd: cmd.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        env: Default::default(),
        cwd: None,
        sudo: false,
        detached: false,
    });

    let mut conn = agent.connect().await?;
    conn.send_request(&request).await?;

    // Drain all responses until we get an exit or the connection closes.
    loop {
        match conn.recv_response().await {
            Ok(AgentResponse::ExecDone { exit_code, .. }) => {
                if exit_code == 0 {
                    return Ok(());
                } else {
                    return Err(RustboxError::AgentComm(format!(
                        "{cmd} exited with code {exit_code}"
                    )));
                }
            }
            Ok(AgentResponse::Output { .. } | AgentResponse::ExecStarted { .. }) => {
                // Ignore output, keep draining.
                continue;
            }
            Ok(AgentResponse::Error { message }) => {
                return Err(RustboxError::AgentComm(message));
            }
            Ok(_) => continue,
            Err(_) => {
                // Connection closed — assume success (fire-and-forget semantics).
                return Ok(());
            }
        }
    }
}
