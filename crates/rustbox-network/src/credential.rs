use crate::domain::domain_matches;
use rustbox_core::network::TransformRule;

/// Check if a domain matches any transform rule and return the matching rule.
pub fn find_credential_headers<'a>(
    domain: &str,
    rules: &'a [TransformRule],
) -> Option<&'a TransformRule> {
    rules.iter().find(|rule| domain_matches(&rule.domain, domain))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_rule(domain: &str, key: &str, value: &str) -> TransformRule {
        let mut headers = HashMap::new();
        headers.insert(key.to_string(), value.to_string());
        TransformRule {
            domain: domain.to_string(),
            headers,
        }
    }

    #[test]
    fn find_matching_rule() {
        let rules = vec![
            make_rule("api.example.com", "Authorization", "Bearer token123"),
            make_rule("other.com", "X-Key", "secret"),
        ];

        let result = find_credential_headers("api.example.com", &rules);
        assert!(result.is_some());
        let rule = result.unwrap();
        assert_eq!(
            rule.headers.get("Authorization").unwrap(),
            "Bearer token123"
        );
    }

    #[test]
    fn no_match_returns_none() {
        let rules = vec![make_rule("api.example.com", "Authorization", "Bearer tok")];

        let result = find_credential_headers("unknown.com", &rules);
        assert!(result.is_none());
    }

    #[test]
    fn wildcard_domain_match() {
        let rules = vec![make_rule(
            "*.example.com",
            "Authorization",
            "Bearer wildcard",
        )];

        let result = find_credential_headers("sub.example.com", &rules);
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().headers.get("Authorization").unwrap(),
            "Bearer wildcard"
        );
    }
}
