use rustbox_core::network::{NetworkMode, NetworkPolicy};

/// Generates nftables rule strings from a NetworkPolicy.
pub struct NftablesRuleSet {
    pub rules: Vec<String>,
}

impl NftablesRuleSet {
    pub fn from_policy(policy: &NetworkPolicy) -> Self {
        let mut rules = Vec::new();

        match policy.mode {
            NetworkMode::AllowAll => {
                // Allow all by default, add specific deny rules
                for subnet in &policy.subnets_deny {
                    rules.push(format!("add rule inet filter output ip daddr {} drop", subnet));
                }
                rules.push("add rule inet filter output accept".to_string());
            }
            NetworkMode::DenyAll => {
                // Deny all by default, add specific allow rules
                // Always allow loopback
                rules.push("add rule inet filter output oif lo accept".to_string());
                // Allow specific subnets
                for subnet in &policy.subnets_allow {
                    rules.push(format!("add rule inet filter output ip daddr {} accept", subnet));
                }
                // Drop everything else
                rules.push("add rule inet filter output drop".to_string());
            }
        }

        Self { rules }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_all_generates_accept() {
        let policy = NetworkPolicy::default();
        let ruleset = NftablesRuleSet::from_policy(&policy);
        assert_eq!(ruleset.rules, vec!["add rule inet filter output accept"]);
    }

    #[test]
    fn deny_all_generates_drop() {
        let policy = NetworkPolicy {
            mode: NetworkMode::DenyAll,
            allow_domains: vec![],
            subnets_allow: vec![],
            subnets_deny: vec![],
            transform_rules: vec![],
        };
        let ruleset = NftablesRuleSet::from_policy(&policy);
        assert_eq!(ruleset.rules, vec![
            "add rule inet filter output oif lo accept",
            "add rule inet filter output drop",
        ]);
    }

    #[test]
    fn deny_specific_subnets() {
        let policy = NetworkPolicy {
            mode: NetworkMode::AllowAll,
            allow_domains: vec![],
            subnets_allow: vec![],
            subnets_deny: vec!["10.0.0.0/8".parse().unwrap()],
            transform_rules: vec![],
        };
        let ruleset = NftablesRuleSet::from_policy(&policy);
        assert_eq!(ruleset.rules, vec![
            "add rule inet filter output ip daddr 10.0.0.0/8 drop",
            "add rule inet filter output accept",
        ]);
    }
}
