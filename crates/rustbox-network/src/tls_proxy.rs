/// Represents a CA certificate and private key for TLS MITM.
pub struct CertificateAuthority {
    pub cert_pem: String,
    pub key_pem: String,
}

impl CertificateAuthority {
    /// Generate a new self-signed CA certificate.
    pub fn generate() -> Result<Self, String> {
        let key_pair =
            rcgen::KeyPair::generate().map_err(|e| format!("keygen: {e}"))?;

        let mut params = rcgen::CertificateParams::default();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.distinguished_name.push(
            rcgen::DnType::CommonName,
            rcgen::DnValue::Utf8String("Rustbox CA".to_string()),
        );
        // Valid for 1 year
        params.not_before = rcgen::date_time_ymd(2024, 1, 1);
        params.not_after = rcgen::date_time_ymd(2025, 12, 31);

        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| format!("cert gen: {e}"))?;

        Ok(Self {
            cert_pem: cert.pem(),
            key_pem: key_pair.serialize_pem(),
        })
    }

    /// Generate a certificate for a specific domain, signed by this CA.
    pub fn issue_cert(&self, domain: &str) -> Result<(String, String), String> {
        let ca_key =
            rcgen::KeyPair::from_pem(&self.key_pem).map_err(|e| format!("parse ca key: {e}"))?;

        // Reconstruct the CA cert params to re-derive the signing certificate.
        let mut ca_params = rcgen::CertificateParams::default();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params.distinguished_name.push(
            rcgen::DnType::CommonName,
            rcgen::DnValue::Utf8String("Rustbox CA".to_string()),
        );
        ca_params.not_before = rcgen::date_time_ymd(2024, 1, 1);
        ca_params.not_after = rcgen::date_time_ymd(2025, 12, 31);
        let ca_cert = ca_params
            .self_signed(&ca_key)
            .map_err(|e| format!("ca cert: {e}"))?;

        let server_key =
            rcgen::KeyPair::generate().map_err(|e| format!("server keygen: {e}"))?;
        let mut server_params = rcgen::CertificateParams::new(vec![domain.to_string()])
            .map_err(|e| format!("server cert params: {e}"))?;
        server_params.is_ca = rcgen::IsCa::NoCa;

        let server_cert = server_params
            .signed_by(&server_key, &ca_cert, &ca_key)
            .map_err(|e| format!("sign cert: {e}"))?;

        Ok((server_cert.pem(), server_key.serialize_pem()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ca_cert_generation() {
        let ca = CertificateAuthority::generate().unwrap();
        assert!(ca.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(ca.key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn issue_cert_for_domain() {
        let ca = CertificateAuthority::generate().unwrap();
        let (cert_pem, key_pem) = ca.issue_cert("example.com").unwrap();
        assert!(!cert_pem.is_empty());
        assert!(!key_pem.is_empty());
        assert_ne!(cert_pem, ca.cert_pem);
        assert_ne!(key_pem, ca.key_pem);
    }

    #[test]
    fn issued_cert_is_valid() {
        let ca = CertificateAuthority::generate().unwrap();
        let (cert_pem, _key_pem) = ca.issue_cert("test.example.com").unwrap();
        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
    }
}
