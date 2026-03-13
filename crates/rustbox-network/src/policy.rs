use rustbox_core::network::{NetworkMode, NetworkPolicy, TransformRule};
use std::net::IpAddr;
use crate::domain::domain_matches;
use crate::cidr::ip_in_any_subnet;

#[derive(Debug)]
pub enum PolicyDecision {
    Allow,
    Deny,
    AllowWithTransform(TransformRule),
}

impl PartialEq for PolicyDecision {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PolicyDecision::Allow, PolicyDecision::Allow) => true,
            (PolicyDecision::Deny, PolicyDecision::Deny) => true,
            (PolicyDecision::AllowWithTransform(a), PolicyDecision::AllowWithTransform(b)) => {
                a.domain == b.domain && a.headers == b.headers
            }
            _ => false,
        }
    }
}

pub struct NetworkPolicyEvaluator {
    policy: NetworkPolicy,
}

impl NetworkPolicyEvaluator {
    pub fn new(policy: NetworkPolicy) -> Self {
        Self { policy }
    }

    pub fn should_allow_domain(&self, domain: &str) -> bool {
        match self.policy.mode {
            NetworkMode::AllowAll => {
                // AllowAll: allow unless explicitly denied
                true
            }
            NetworkMode::DenyAll => {
                // DenyAll: only allow if in allow_domains
                self.policy.allow_domains.iter().any(|pattern| domain_matches(pattern, domain))
            }
        }
    }

    pub fn should_allow_ip(&self, ip: IpAddr) -> bool {
        // Check deny list first
        if ip_in_any_subnet(ip, &self.policy.subnets_deny) {
            return false;
        }
        match self.policy.mode {
            NetworkMode::AllowAll => true,
            NetworkMode::DenyAll => {
                ip_in_any_subnet(ip, &self.policy.subnets_allow)
            }
        }
    }

    pub fn evaluate_connection(&self, domain: &str, ip: IpAddr) -> PolicyDecision {
        // Check transform rules first
        for rule in &self.policy.transform_rules {
            if domain_matches(&rule.domain, domain) {
                return PolicyDecision::AllowWithTransform(rule.clone());
            }
        }

        if !self.should_allow_domain(domain) {
            return PolicyDecision::Deny;
        }
        if !self.should_allow_ip(ip) {
            return PolicyDecision::Deny;
        }
        PolicyDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn allow_all_allows_any() {
        let policy = NetworkPolicy::default(); // AllowAll
        let evaluator = NetworkPolicyEvaluator::new(policy);
        assert!(evaluator.should_allow_domain("anything.example.com"));
    }

    #[test]
    fn deny_all_denies_by_default() {
        let policy = NetworkPolicy {
            mode: NetworkMode::DenyAll,
            allow_domains: vec![],
            subnets_allow: vec![],
            subnets_deny: vec![],
            transform_rules: vec![],
        };
        let evaluator = NetworkPolicyEvaluator::new(policy);
        assert!(!evaluator.should_allow_domain("example.com"));
    }

    #[test]
    fn deny_all_with_allowlist() {
        let policy = NetworkPolicy {
            mode: NetworkMode::DenyAll,
            allow_domains: vec!["example.com".to_string()],
            subnets_allow: vec![],
            subnets_deny: vec![],
            transform_rules: vec![],
        };
        let evaluator = NetworkPolicyEvaluator::new(policy);
        assert!(evaluator.should_allow_domain("example.com"));
        assert!(!evaluator.should_allow_domain("other.com"));
    }

    #[test]
    fn allow_all_subnet_deny() {
        let policy = NetworkPolicy {
            mode: NetworkMode::AllowAll,
            allow_domains: vec![],
            subnets_allow: vec![],
            subnets_deny: vec!["192.168.1.0/24".parse().unwrap()],
            transform_rules: vec![],
        };
        let evaluator = NetworkPolicyEvaluator::new(policy);
        let ip: IpAddr = "192.168.1.5".parse().unwrap();
        assert!(!evaluator.should_allow_ip(ip));
    }

    #[test]
    fn evaluate_connection_transform() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer tok".to_string());
        let rule = TransformRule {
            domain: "api.example.com".to_string(),
            headers: headers.clone(),
        };
        let policy = NetworkPolicy {
            mode: NetworkMode::AllowAll,
            allow_domains: vec![],
            subnets_allow: vec![],
            subnets_deny: vec![],
            transform_rules: vec![rule],
        };
        let evaluator = NetworkPolicyEvaluator::new(policy);
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        let decision = evaluator.evaluate_connection("api.example.com", ip);
        let expected_rule = TransformRule {
            domain: "api.example.com".to_string(),
            headers,
        };
        assert_eq!(decision, PolicyDecision::AllowWithTransform(expected_rule));
    }
}
