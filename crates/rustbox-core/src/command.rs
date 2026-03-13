use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CommandRequest {
    pub cmd: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub sudo: bool,
    pub detached: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CommandOutput {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(i32),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Running,
    Completed(i32),
    Failed(String),
    Killed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn command_request_serde_roundtrip() {
        let mut env = HashMap::new();
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        let req = CommandRequest {
            cmd: "ls".to_string(),
            args: vec!["-la".to_string()],
            cwd: Some("/tmp".to_string()),
            env: Some(env),
            sudo: false,
            detached: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CommandRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cmd, "ls");
        assert_eq!(back.args, vec!["-la"]);
        assert_eq!(back.cwd.as_deref(), Some("/tmp"));
        assert!(back.env.is_some());
        assert!(!back.sudo);
        assert!(back.detached);
    }

    #[test]
    fn command_output_stdout_roundtrip() {
        let out = CommandOutput::Stdout(vec![72, 101, 108, 108, 111]);
        let json = serde_json::to_string(&out).unwrap();
        assert!(json.contains("\"stdout\""));
        let back: CommandOutput = serde_json::from_str(&json).unwrap();
        match back {
            CommandOutput::Stdout(data) => assert_eq!(data, vec![72, 101, 108, 108, 111]),
            _ => panic!("expected Stdout"),
        }
    }

    #[test]
    fn command_output_stderr_roundtrip() {
        let out = CommandOutput::Stderr(vec![69, 114, 114]);
        let json = serde_json::to_string(&out).unwrap();
        let back: CommandOutput = serde_json::from_str(&json).unwrap();
        match back {
            CommandOutput::Stderr(data) => assert_eq!(data, vec![69, 114, 114]),
            _ => panic!("expected Stderr"),
        }
    }

    #[test]
    fn command_output_exit_roundtrip() {
        let out = CommandOutput::Exit(0);
        let json = serde_json::to_string(&out).unwrap();
        let back: CommandOutput = serde_json::from_str(&json).unwrap();
        match back {
            CommandOutput::Exit(code) => assert_eq!(code, 0),
            _ => panic!("expected Exit"),
        }
    }

    #[test]
    fn command_status_running_roundtrip() {
        let s = CommandStatus::Running;
        let json = serde_json::to_string(&s).unwrap();
        let back: CommandStatus = serde_json::from_str(&json).unwrap();
        match back {
            CommandStatus::Running => {}
            _ => panic!("expected Running"),
        }
    }

    #[test]
    fn command_status_completed_roundtrip() {
        let s = CommandStatus::Completed(42);
        let json = serde_json::to_string(&s).unwrap();
        let back: CommandStatus = serde_json::from_str(&json).unwrap();
        match back {
            CommandStatus::Completed(code) => assert_eq!(code, 42),
            _ => panic!("expected Completed"),
        }
    }

    #[test]
    fn command_status_failed_roundtrip() {
        let s = CommandStatus::Failed("segfault".to_string());
        let json = serde_json::to_string(&s).unwrap();
        let back: CommandStatus = serde_json::from_str(&json).unwrap();
        match back {
            CommandStatus::Failed(msg) => assert_eq!(msg, "segfault"),
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn command_status_killed_roundtrip() {
        let s = CommandStatus::Killed;
        let json = serde_json::to_string(&s).unwrap();
        let back: CommandStatus = serde_json::from_str(&json).unwrap();
        match back {
            CommandStatus::Killed => {}
            _ => panic!("expected Killed"),
        }
    }
}
