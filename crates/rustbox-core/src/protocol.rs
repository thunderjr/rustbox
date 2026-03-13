use serde::{Deserialize, Serialize};

use crate::command::CommandRequest;
use crate::metrics::SandboxMetrics;

/// Guest agent request messages, sent over vsock as length-prefixed JSON.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRequest {
    Exec(CommandRequest),
    Kill {
        command_id: String,
        signal: i32,
    },
    /// Note: `content` is raw bytes; consider base64 encoding for JSON transport.
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

/// Guest agent response messages, sent over vsock as length-prefixed JSON.
#[derive(Serialize, Deserialize, Clone, Debug)]
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
    MetricsResult(SandboxMetrics),
    Pong,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::CommandRequest;
    use serde_json;

    #[test]
    fn agent_request_exec_tagged() {
        let req = AgentRequest::Exec(CommandRequest {
            cmd: "echo".to_string(),
            args: vec!["hi".to_string()],
            cwd: None,
            env: None,
            sudo: false,
            detached: false,
        });
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["type"], "exec");
        assert_eq!(json["cmd"], "echo");
    }

    #[test]
    fn agent_request_ping_tagged() {
        let req = AgentRequest::Ping;
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["type"], "ping");
    }

    #[test]
    fn agent_request_kill_tagged() {
        let req = AgentRequest::Kill {
            command_id: "abc-123".to_string(),
            signal: 9,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["type"], "kill");
        assert_eq!(json["command_id"], "abc-123");
        assert_eq!(json["signal"], 9);
    }

    #[test]
    fn agent_request_write_file_tagged() {
        let req = AgentRequest::WriteFile {
            path: "/tmp/test.txt".to_string(),
            content: vec![65, 66, 67],
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["type"], "write_file");
        assert_eq!(json["path"], "/tmp/test.txt");
    }

    #[test]
    fn agent_request_read_file_tagged() {
        let req = AgentRequest::ReadFile {
            path: "/etc/hosts".to_string(),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["type"], "read_file");
    }

    #[test]
    fn agent_request_mkdir_tagged() {
        let req = AgentRequest::Mkdir {
            path: "/tmp/newdir".to_string(),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["type"], "mkdir");
    }

    #[test]
    fn agent_request_metrics_tagged() {
        let req = AgentRequest::Metrics;
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["type"], "metrics");
    }

    #[test]
    fn agent_request_roundtrip() {
        let req = AgentRequest::Kill {
            command_id: "test-id".to_string(),
            signal: 15,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: AgentRequest = serde_json::from_str(&json).unwrap();
        match back {
            AgentRequest::Kill { command_id, signal } => {
                assert_eq!(command_id, "test-id");
                assert_eq!(signal, 15);
            }
            _ => panic!("expected Kill"),
        }
    }

    #[test]
    fn agent_response_exec_started_roundtrip() {
        let resp = AgentResponse::ExecStarted {
            command_id: "cmd-1".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "exec_started");
        let back: AgentResponse = serde_json::from_str(&json).unwrap();
        match back {
            AgentResponse::ExecStarted { command_id } => assert_eq!(command_id, "cmd-1"),
            _ => panic!("expected ExecStarted"),
        }
    }

    #[test]
    fn agent_response_output_roundtrip() {
        let resp = AgentResponse::Output {
            command_id: "cmd-2".to_string(),
            stream: OutputStream::Stdout,
            data: vec![104, 105],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: AgentResponse = serde_json::from_str(&json).unwrap();
        match back {
            AgentResponse::Output {
                command_id,
                data,
                ..
            } => {
                assert_eq!(command_id, "cmd-2");
                assert_eq!(data, vec![104, 105]);
            }
            _ => panic!("expected Output"),
        }
    }

    #[test]
    fn agent_response_exec_done_roundtrip() {
        let resp = AgentResponse::ExecDone {
            command_id: "cmd-3".to_string(),
            exit_code: 0,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: AgentResponse = serde_json::from_str(&json).unwrap();
        match back {
            AgentResponse::ExecDone {
                command_id,
                exit_code,
            } => {
                assert_eq!(command_id, "cmd-3");
                assert_eq!(exit_code, 0);
            }
            _ => panic!("expected ExecDone"),
        }
    }

    #[test]
    fn agent_response_ok_roundtrip() {
        let resp = AgentResponse::Ok;
        let json = serde_json::to_string(&resp).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "ok");
        let back: AgentResponse = serde_json::from_str(&json).unwrap();
        match back {
            AgentResponse::Ok => {}
            _ => panic!("expected Ok"),
        }
    }

    #[test]
    fn agent_response_error_roundtrip() {
        let resp = AgentResponse::Error {
            message: "something broke".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: AgentResponse = serde_json::from_str(&json).unwrap();
        match back {
            AgentResponse::Error { message } => assert_eq!(message, "something broke"),
            _ => panic!("expected Error"),
        }
    }

    #[test]
    fn agent_response_pong_roundtrip() {
        let resp = AgentResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "pong");
        let back: AgentResponse = serde_json::from_str(&json).unwrap();
        match back {
            AgentResponse::Pong => {}
            _ => panic!("expected Pong"),
        }
    }

    #[test]
    fn agent_response_file_content_roundtrip() {
        let resp = AgentResponse::FileContent {
            data: vec![1, 2, 3],
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: AgentResponse = serde_json::from_str(&json).unwrap();
        match back {
            AgentResponse::FileContent { data } => assert_eq!(data, vec![1, 2, 3]),
            _ => panic!("expected FileContent"),
        }
    }

    #[test]
    fn output_stream_serde() {
        let stdout = serde_json::to_string(&OutputStream::Stdout).unwrap();
        assert_eq!(stdout, "\"stdout\"");
        let stderr = serde_json::to_string(&OutputStream::Stderr).unwrap();
        assert_eq!(stderr, "\"stderr\"");
    }
}
