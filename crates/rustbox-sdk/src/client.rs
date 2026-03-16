use crate::error::{Result, SdkError};
use chrono::{DateTime, Utc};
use reqwest::Client;
use rustbox_core::command::CommandStatus;
use rustbox_core::network::NetworkPolicy;
use rustbox_core::sandbox::{CpuCount, Runtime, SandboxSource, SandboxStatus};
use rustbox_core::SandboxMetrics;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub struct RustboxClient {
    base_url: String,
    client: Client,
}

// Mirror the daemon's DTOs for serialization
#[derive(Serialize)]
struct CreateSandboxBody {
    runtime: Runtime,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpu_count: Option<CpuCount>,
    timeout_secs: u64,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    env: HashMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ports: Vec<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<SandboxSource>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct SandboxInfo {
    pub id: String,
    pub status: SandboxStatus,
    pub runtime: Runtime,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize, Debug)]
struct ExecResponseBody {
    command_id: String,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct CommandOutputEntry {
    pub stream: String,
    pub data: Option<Vec<u8>>,
    pub exit_code: Option<i32>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct CommandInfo {
    pub command_id: String,
    pub status: CommandStatus,
    pub output: Vec<CommandOutputEntry>,
}

#[derive(Deserialize, Debug)]
struct ReadFileBody {
    #[allow(dead_code)]
    path: String,
    content: Vec<u8>,
}

#[derive(Deserialize, Serialize, Debug)]
pub struct SnapshotInfo {
    pub id: String,
    pub sandbox_id: String,
    pub created_at: DateTime<Utc>,
    pub size_bytes: u64,
    pub description: Option<String>,
}

impl RustboxClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client: Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/v1{}", self.base_url, path)
    }

    async fn check_error(&self, resp: reqwest::Response) -> Result<reqwest::Response> {
        let status = resp.status();
        if status.is_success() {
            Ok(resp)
        } else {
            let body = resp.text().await.unwrap_or_default();
            // Try to extract error message from JSON
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(String::from))
                .unwrap_or(body);
            Err(SdkError::from_status(status, message))
        }
    }

    pub async fn create_sandbox(
        &self,
        runtime: Runtime,
        timeout_secs: u64,
    ) -> Result<SandboxInfo> {
        let body = CreateSandboxBody {
            runtime,
            cpu_count: None,
            timeout_secs,
            env: HashMap::new(),
            ports: vec![],
            source: None,
        };
        let resp = self
            .client
            .post(self.url("/sandboxes"))
            .json(&body)
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn get_sandbox(&self, id: &str) -> Result<SandboxInfo> {
        let resp = self
            .client
            .get(self.url(&format!("/sandboxes/{id}")))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn list_sandboxes(&self) -> Result<Vec<SandboxInfo>> {
        let resp = self
            .client
            .get(self.url("/sandboxes"))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn delete_sandbox(&self, id: &str) -> Result<()> {
        let resp = self
            .client
            .delete(self.url(&format!("/sandboxes/{id}")))
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    pub async fn update_timeout(&self, id: &str, timeout_secs: u64) -> Result<SandboxInfo> {
        let resp = self
            .client
            .patch(self.url(&format!("/sandboxes/{id}/timeout")))
            .json(&serde_json::json!({ "timeout_secs": timeout_secs }))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn update_network_policy(
        &self,
        id: &str,
        policy: NetworkPolicy,
    ) -> Result<SandboxInfo> {
        let resp = self
            .client
            .patch(self.url(&format!("/sandboxes/{id}/network-policy")))
            .json(&serde_json::json!({ "network_policy": policy }))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn exec(
        &self,
        sandbox_id: &str,
        cmd: &str,
        args: &[&str],
    ) -> Result<String> {
        let body = serde_json::json!({
            "cmd": cmd,
            "args": args,
        });
        let resp = self
            .client
            .post(self.url(&format!("/sandboxes/{sandbox_id}/commands")))
            .json(&body)
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        let body: ExecResponseBody = resp.json().await?;
        Ok(body.command_id)
    }

    pub async fn get_command(
        &self,
        sandbox_id: &str,
        cmd_id: &str,
    ) -> Result<CommandInfo> {
        let resp = self
            .client
            .get(self.url(&format!("/sandboxes/{sandbox_id}/commands/{cmd_id}")))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn kill_command(
        &self,
        sandbox_id: &str,
        cmd_id: &str,
    ) -> Result<()> {
        let resp = self
            .client
            .post(self.url(&format!(
                "/sandboxes/{sandbox_id}/commands/{cmd_id}/kill"
            )))
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    pub async fn upload_file(
        &self,
        sandbox_id: &str,
        path: &str,
        content: &[u8],
    ) -> Result<()> {
        let body = serde_json::json!({
            "path": path,
            "content": content,
        });
        let resp = self
            .client
            .post(self.url(&format!("/sandboxes/{sandbox_id}/files")))
            .json(&body)
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    pub async fn download_file(
        &self,
        sandbox_id: &str,
        path: &str,
    ) -> Result<Vec<u8>> {
        let resp = self
            .client
            .get(self.url(&format!("/sandboxes/{sandbox_id}/files")))
            .query(&[("path", path)])
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        let body: ReadFileBody = resp.json().await?;
        Ok(body.content)
    }

    pub async fn mkdir(
        &self,
        sandbox_id: &str,
        path: &str,
    ) -> Result<()> {
        let body = serde_json::json!({ "path": path });
        let resp = self
            .client
            .post(self.url(&format!("/sandboxes/{sandbox_id}/dirs")))
            .json(&body)
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    pub async fn create_snapshot(
        &self,
        sandbox_id: &str,
        description: Option<&str>,
    ) -> Result<SnapshotInfo> {
        let body = serde_json::json!({
            "sandbox_id": sandbox_id,
            "description": description,
        });
        let resp = self
            .client
            .post(self.url("/snapshots"))
            .json(&body)
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn get_snapshot(&self, id: &str) -> Result<SnapshotInfo> {
        let resp = self
            .client
            .get(self.url(&format!("/snapshots/{id}")))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn delete_snapshot(&self, id: &str) -> Result<()> {
        let resp = self
            .client
            .delete(self.url(&format!("/snapshots/{id}")))
            .send()
            .await?;
        self.check_error(resp).await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn exec_full(
        &self,
        sandbox_id: &str,
        cmd: &str,
        args: &[String],
        cwd: Option<&str>,
        env: Option<&HashMap<String, String>>,
        sudo: bool,
        detached: bool,
    ) -> Result<String> {
        let body = serde_json::json!({
            "cmd": cmd,
            "args": args,
            "cwd": cwd,
            "env": env,
            "sudo": sudo,
            "detached": detached,
        });
        let resp = self
            .client
            .post(self.url(&format!("/sandboxes/{sandbox_id}/commands")))
            .json(&body)
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        let body: ExecResponseBody = resp.json().await?;
        Ok(body.command_id)
    }

    pub async fn list_snapshots(&self) -> Result<Vec<SnapshotInfo>> {
        let resp = self
            .client
            .get(self.url("/snapshots"))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }

    pub async fn get_metrics(&self, sandbox_id: &str) -> Result<SandboxMetrics> {
        let resp = self
            .client
            .get(self.url(&format!("/sandboxes/{sandbox_id}/metrics")))
            .send()
            .await?;
        let resp = self.check_error(resp).await?;
        Ok(resp.json().await?)
    }
}
