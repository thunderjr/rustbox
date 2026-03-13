use ipnet::IpNet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    AllowAll,
    DenyAll,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NetworkPolicy {
    pub mode: NetworkMode,
    pub allow_domains: Vec<String>,
    pub subnets_allow: Vec<IpNet>,
    pub subnets_deny: Vec<IpNet>,
    pub transform_rules: Vec<TransformRule>,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            mode: NetworkMode::AllowAll,
            allow_domains: Vec::new(),
            subnets_allow: Vec::new(),
            subnets_deny: Vec::new(),
            transform_rules: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransformRule {
    pub domain: String,
    pub headers: HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn network_policy_default_is_allow_all_empty_vecs() {
        let policy = NetworkPolicy::default();
        match policy.mode {
            NetworkMode::AllowAll => {}
            _ => panic!("expected AllowAll"),
        }
        assert!(policy.allow_domains.is_empty());
        assert!(policy.subnets_allow.is_empty());
        assert!(policy.subnets_deny.is_empty());
        assert!(policy.transform_rules.is_empty());
    }

    #[test]
    fn network_policy_serde_roundtrip() {
        let policy = NetworkPolicy {
            mode: NetworkMode::DenyAll,
            allow_domains: vec!["example.com".to_string()],
            subnets_allow: vec!["10.0.0.0/8".parse().unwrap()],
            subnets_deny: vec!["192.168.1.0/24".parse().unwrap()],
            transform_rules: vec![TransformRule {
                domain: "api.example.com".to_string(),
                headers: {
                    let mut h = HashMap::new();
                    h.insert("Authorization".to_string(), "Bearer tok".to_string());
                    h
                },
            }],
        };
        let json = serde_json::to_string(&policy).unwrap();
        let back: NetworkPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(back.allow_domains, vec!["example.com"]);
        assert_eq!(back.subnets_allow.len(), 1);
        assert_eq!(back.subnets_deny.len(), 1);
        assert_eq!(back.transform_rules.len(), 1);
    }

    #[test]
    fn transform_rule_serde_roundtrip() {
        let mut headers = HashMap::new();
        headers.insert("X-Custom".to_string(), "value".to_string());
        let rule = TransformRule {
            domain: "test.com".to_string(),
            headers,
        };
        let json = serde_json::to_string(&rule).unwrap();
        let back: TransformRule = serde_json::from_str(&json).unwrap();
        assert_eq!(back.domain, "test.com");
        assert_eq!(back.headers.get("X-Custom").unwrap(), "value");
    }

    #[test]
    fn network_mode_serde_variants() {
        let allow = serde_json::to_string(&NetworkMode::AllowAll).unwrap();
        assert_eq!(allow, "\"allow_all\"");
        let deny = serde_json::to_string(&NetworkMode::DenyAll).unwrap();
        assert_eq!(deny, "\"deny_all\"");
    }
}
