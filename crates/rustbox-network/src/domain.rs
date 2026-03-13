/// Check if a domain matches a pattern.
/// Patterns can be exact ("example.com") or wildcard ("*.example.com").
/// Wildcard matches any subdomain but NOT the bare domain itself.
/// Matching is case-insensitive.
pub fn domain_matches(pattern: &str, domain: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let domain = domain.to_lowercase();

    if let Some(suffix) = pattern.strip_prefix("*.") {
        // Wildcard: matches any subdomain of suffix, but not suffix itself
        if domain == suffix {
            return false;
        }
        domain.ends_with(&format!(".{suffix}"))
    } else {
        // Exact match
        domain == pattern
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        assert!(domain_matches("example.com", "example.com"));
    }

    #[test]
    fn no_match() {
        assert!(!domain_matches("example.com", "other.com"));
    }

    #[test]
    fn wildcard_match() {
        assert!(domain_matches("*.example.com", "sub.example.com"));
    }

    #[test]
    fn wildcard_rejects_bare() {
        assert!(!domain_matches("*.example.com", "example.com"));
    }

    #[test]
    fn nested_subdomain() {
        assert!(domain_matches("*.example.com", "a.b.example.com"));
    }

    #[test]
    fn case_insensitive() {
        assert!(domain_matches("*.Example.COM", "Sub.EXAMPLE.com"));
    }
}
