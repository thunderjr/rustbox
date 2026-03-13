use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use crate::id::{SandboxId, SnapshotId};
use crate::network::NetworkPolicy;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SandboxStatus {
    Pending,
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Node24,
    Node22,
    Python313,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Copy)]
#[serde(rename_all = "snake_case")]
pub enum CpuCount {
    One = 1,
    Two = 2,
    Four = 4,
    Eight = 8,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SandboxConfig {
    pub runtime: Runtime,
    pub cpu_count: CpuCount,
    #[serde(with = "duration_secs")]
    pub timeout: Duration,
    pub env: HashMap<String, String>,
    pub ports: Vec<u16>,
    pub network_policy: NetworkPolicy,
    pub source: Option<SandboxSource>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SandboxSource {
    Git {
        url: String,
        username: Option<String>,
        password: Option<String>,
        depth: Option<u32>,
        revision: Option<String>,
    },
    Tarball {
        url: String,
    },
    Snapshot(SnapshotId),
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Sandbox {
    pub id: SandboxId,
    pub config: SandboxConfig,
    pub status: SandboxStatus,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
}

/// Serialize/deserialize Duration as seconds (u64).
mod duration_secs {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(Duration::from_secs(secs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn sandbox_status_serde_roundtrip() {
        let variants = vec![
            SandboxStatus::Pending,
            SandboxStatus::Running,
            SandboxStatus::Stopping,
            SandboxStatus::Stopped,
            SandboxStatus::Failed,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let back: SandboxStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn sandbox_status_snake_case() {
        let json = serde_json::to_string(&SandboxStatus::Running).unwrap();
        assert_eq!(json, "\"running\"");
    }

    #[test]
    fn runtime_serde_roundtrip() {
        let variants = vec![Runtime::Node24, Runtime::Node22, Runtime::Python313];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let back: Runtime = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn cpu_count_serde_roundtrip() {
        let variants = vec![CpuCount::One, CpuCount::Two, CpuCount::Four, CpuCount::Eight];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let back: CpuCount = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn duration_serializes_as_seconds() {
        // Build a config with a known duration and check the JSON number
        let config = SandboxConfig {
            runtime: Runtime::Node24,
            cpu_count: CpuCount::One,
            timeout: Duration::from_secs(300),
            env: HashMap::new(),
            ports: vec![],
            network_policy: NetworkPolicy::default(),
            source: None,
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["timeout"], serde_json::json!(300));
    }

    #[test]
    fn sandbox_config_full_roundtrip() {
        let mut env = HashMap::new();
        env.insert("NODE_ENV".to_string(), "production".to_string());

        let config = SandboxConfig {
            runtime: Runtime::Python313,
            cpu_count: CpuCount::Four,
            timeout: Duration::from_secs(600),
            env,
            ports: vec![8080, 3000],
            network_policy: NetworkPolicy::default(),
            source: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        let back: SandboxConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.runtime, config.runtime);
        assert_eq!(back.cpu_count, config.cpu_count);
        assert_eq!(back.timeout, config.timeout);
        assert_eq!(back.env.get("NODE_ENV").unwrap(), "production");
        assert_eq!(back.ports, vec![8080, 3000]);
    }

    #[test]
    fn sandbox_source_git_roundtrip() {
        let src = SandboxSource::Git {
            url: "https://github.com/test/repo".to_string(),
            username: Some("user".to_string()),
            password: None,
            depth: Some(1),
            revision: Some("main".to_string()),
        };
        let json = serde_json::to_string(&src).unwrap();
        assert!(json.contains("\"type\":\"git\""));
        let back: SandboxSource = serde_json::from_str(&json).unwrap();
        match back {
            SandboxSource::Git { url, depth, .. } => {
                assert_eq!(url, "https://github.com/test/repo");
                assert_eq!(depth, Some(1));
            }
            _ => panic!("expected Git variant"),
        }
    }

    #[test]
    fn sandbox_source_tarball_roundtrip() {
        let src = SandboxSource::Tarball {
            url: "https://example.com/archive.tar.gz".to_string(),
        };
        let json = serde_json::to_string(&src).unwrap();
        assert!(json.contains("\"type\":\"tarball\""));
        let back: SandboxSource = serde_json::from_str(&json).unwrap();
        match back {
            SandboxSource::Tarball { url } => {
                assert_eq!(url, "https://example.com/archive.tar.gz");
            }
            _ => panic!("expected Tarball variant"),
        }
    }
}
