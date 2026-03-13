//! Agent protocol types.
//!
//! These are defined locally (rather than depending on `rustbox-core`) so that
//! the agent binary stays minimal and can be statically linked for the guest VM.
//! The serde representation **must** stay wire-compatible with the corresponding
//! types in `rustbox-core::protocol`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Requests (host -> agent)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRequest {
    Exec {
        cmd: String,
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        env: Option<HashMap<String, String>>,
        #[serde(default)]
        sudo: bool,
        #[serde(default)]
        detached: bool,
    },
    Kill {
        command_id: String,
        signal: i32,
    },
    WriteFile {
        path: String,
        content: Vec<u8>,
    },
    ReadFile {
        path: String,
    },
    Mkdir {
        path: String,
    },
    Metrics,
    Ping,
}

// ---------------------------------------------------------------------------
// Responses (agent -> host)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentResponse {
    ExecStarted {
        command_id: String,
    },
    Output {
        command_id: String,
        stream: OutputStream,
        data: Vec<u8>,
    },
    ExecDone {
        command_id: String,
        exit_code: i32,
    },
    FileContent {
        data: Vec<u8>,
    },
    Ok,
    Error {
        message: String,
    },
    MetricsResult {
        cpu_usage_percent: f64,
        memory_used_bytes: u64,
        memory_total_bytes: u64,
        network_rx_bytes: u64,
        network_tx_bytes: u64,
        disk_used_bytes: u64,
    },
    Pong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}
