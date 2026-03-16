use rustbox_core::sandbox::{Runtime, CpuCount, SandboxSource, SandboxStatus};
use rustbox_core::network::NetworkPolicy;
use rustbox_core::command::CommandStatus;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use chrono::{DateTime, Utc};

#[derive(Deserialize)]
pub struct CreateSandboxRequest {
    pub runtime: Runtime,
    #[serde(default = "default_cpu")]
    pub cpu_count: CpuCount,
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub ports: Vec<u16>,
    #[serde(default)]
    pub network_policy: NetworkPolicy,
    pub source: Option<SandboxSource>,
}

fn default_cpu() -> CpuCount {
    CpuCount::One
}

fn default_timeout() -> u64 {
    300
}

#[derive(Serialize)]
pub struct SandboxResponse {
    pub id: String,
    pub status: SandboxStatus,
    pub runtime: Runtime,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
pub struct ExecRequest {
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    #[serde(default)]
    pub sudo: bool,
    #[serde(default)]
    pub detached: bool,
}

#[derive(Serialize)]
pub struct ExecResponse {
    pub command_id: String,
}

#[derive(Serialize)]
pub struct CommandResponse {
    pub command_id: String,
    pub status: CommandStatus,
    pub output: Vec<CommandOutputEntry>,
}

#[derive(Serialize)]
pub struct CommandOutputEntry {
    pub stream: String,
    pub data: Option<Vec<u8>>,
    pub exit_code: Option<i32>,
}

#[derive(Deserialize)]
pub struct WriteFileRequest {
    pub path: String,
    pub content: Vec<u8>,
}

#[derive(Serialize)]
pub struct ReadFileResponse {
    pub path: String,
    pub content: Vec<u8>,
}

#[derive(Deserialize)]
pub struct MkdirRequest {
    pub path: String,
}

#[derive(Deserialize)]
pub struct CreateSnapshotRequest {
    pub sandbox_id: String,
    pub description: Option<String>,
}

#[derive(Serialize)]
pub struct SnapshotResponse {
    pub id: String,
    pub sandbox_id: String,
    pub created_at: DateTime<Utc>,
    pub size_bytes: u64,
    pub description: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateTimeoutRequest {
    pub timeout_secs: u64,
}

#[derive(Deserialize)]
pub struct UpdateNetworkPolicyRequest {
    pub network_policy: NetworkPolicy,
}
