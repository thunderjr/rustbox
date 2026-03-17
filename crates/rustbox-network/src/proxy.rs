//! Transparent HTTP/HTTPS proxy for domain-level network filtering.
//!
//! Routes sandbox traffic (ports 80, 443) through a host-side proxy that uses
//! `NetworkPolicyEvaluator` for allow/deny decisions and `CertificateAuthority`
//! for HTTPS MITM when domain-level rules are present.
//!
//! Linux-only — gated behind `#[cfg(target_os = "linux")]` in lib.rs.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use rustbox_core::network::NetworkPolicy;
use crate::policy::{NetworkPolicyEvaluator, PolicyDecision};
use crate::tls_proxy::CertificateAuthority;

/// A transparent HTTP/HTTPS proxy that enforces domain-level network policy.
///
/// Activates only when domain-level rules exist (i.e., `allow_domains` non-empty
/// in DenyAll mode, or `transform_rules` non-empty). Subnet-only policies skip
/// the proxy entirely.
pub struct TransparentProxy {
    evaluator: Arc<RwLock<NetworkPolicyEvaluator>>,
    ca: Arc<CertificateAuthority>,
    listener_handle: Option<JoinHandle<()>>,
    bind_addr: SocketAddr,
}

/// Check whether a policy requires the domain-level proxy.
pub fn needs_domain_proxy(policy: &NetworkPolicy) -> bool {
    !policy.transform_rules.is_empty()
        || (!policy.allow_domains.is_empty()
            && matches!(
                policy.mode,
                rustbox_core::network::NetworkMode::DenyAll
            ))
}

impl TransparentProxy {
    /// Start the transparent proxy, binding to `127.0.0.1:0` (OS-assigned port).
    pub async fn start(policy: NetworkPolicy, ca: CertificateAuthority) -> io::Result<Self> {
        let evaluator = Arc::new(RwLock::new(NetworkPolicyEvaluator::new(policy)));
        let ca = Arc::new(ca);

        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let bind_addr = listener.local_addr()?;

        info!(addr = %bind_addr, "transparent proxy started");

        let evaluator_clone = evaluator.clone();
        let ca_clone = ca.clone();

        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer)) => {
                        let eval = evaluator_clone.clone();
                        let ca = ca_clone.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, peer, eval, ca).await {
                                debug!(peer = %peer, error = %e, "connection handler error");
                            }
                        });
                    }
                    Err(e) => {
                        error!(error = %e, "accept error");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            evaluator,
            ca,
            listener_handle: Some(handle),
            bind_addr,
        })
    }

    /// Stop the proxy.
    pub fn stop(mut self) {
        if let Some(handle) = self.listener_handle.take() {
            handle.abort();
        }
    }

    /// Swap the policy evaluator for runtime updates.
    pub async fn update_policy(&self, policy: NetworkPolicy) {
        let mut eval = self.evaluator.write().await;
        *eval = NetworkPolicyEvaluator::new(policy);
    }

    /// Return the bound port (for iptables REDIRECT rules).
    pub fn port(&self) -> u16 {
        self.bind_addr.port()
    }

    /// Return a reference to the CA (for writing the cert to the guest).
    pub fn ca(&self) -> &CertificateAuthority {
        &self.ca
    }
}

impl Drop for TransparentProxy {
    fn drop(&mut self) {
        if let Some(handle) = self.listener_handle.take() {
            handle.abort();
        }
    }
}

/// A no-op TLS certificate verifier for upstream connections.
///
/// Since this proxy performs MITM for header injection, the proxy itself is
/// the trust boundary. The guest trusts the proxy's CA, and the proxy connects
/// to the real upstream without verification.
#[derive(Debug)]
struct NoVerify;

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::aws_lc_rs::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Handle a single proxied connection.
///
/// Peeks the first bytes to determine protocol:
/// - HTTP CONNECT → HTTPS (TLS MITM)
/// - Otherwise → plain HTTP
async fn handle_connection(
    stream: TcpStream,
    peer: SocketAddr,
    evaluator: Arc<RwLock<NetworkPolicyEvaluator>>,
    ca: Arc<CertificateAuthority>,
) -> io::Result<()> {
    let mut peek_buf = [0u8; 8];
    let n = stream.peek(&mut peek_buf).await?;
    if n == 0 {
        return Ok(());
    }

    // HTTP CONNECT starts with "CONNECT "
    if peek_buf.starts_with(b"CONNECT ") {
        handle_https_connect(stream, peer, evaluator, ca).await
    } else {
        handle_plain_http(stream, peer, evaluator).await
    }
}

/// Maximum header size before returning 431 Request Header Fields Too Large.
const MAX_HEADER_SIZE: usize = 65536;

/// Read from `stream` until `\r\n\r\n` is found or `MAX_HEADER_SIZE` is exceeded.
/// Returns the accumulated bytes (headers + any body bytes read so far).
async fn read_until_headers_end(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            if buf.is_empty() {
                return Ok(buf);
            }
            // Connection closed before headers ended — return what we have
            // and let the caller deal with the parse error.
            return Ok(buf);
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > MAX_HEADER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request headers too large",
            ));
        }
    }
    Ok(buf)
}

/// Handle plain HTTP: parse Host header, evaluate policy, forward or deny.
async fn handle_plain_http(
    mut stream: TcpStream,
    _peer: SocketAddr,
    evaluator: Arc<RwLock<NetworkPolicyEvaluator>>,
) -> io::Result<()> {
    // Read until we have the full headers (up to 64KB).
    let buf = match read_until_headers_end(&mut stream).await {
        Ok(b) if b.is_empty() => return Ok(()),
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            send_http_error(&mut stream, 431, "Request Header Fields Too Large").await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let request_data = &buf;

    // Parse headers to extract Host.
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    let _status = req.parse(request_data).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("HTTP parse error: {e}"))
    })?;

    let host = extract_host(&req).unwrap_or_default();
    let domain = host.split(':').next().unwrap_or(host);

    if domain.is_empty() {
        send_http_error(&mut stream, 400, "Bad Request: missing Host header").await?;
        return Ok(());
    }

    // Resolve domain to IP for policy evaluation.
    let ip = resolve_domain(domain).await;
    let eval = evaluator.read().await;

    let decision = match ip {
        Some(addr) => eval.evaluate_connection(domain, addr),
        None => {
            // Can't resolve — check domain-only policy.
            if eval.should_allow_domain(domain) {
                PolicyDecision::Allow
            } else {
                PolicyDecision::Deny
            }
        }
    };

    // Get transform rules before dropping the lock.
    let transform_rules = match &decision {
        PolicyDecision::AllowWithTransform(rule) => Some(rule.clone()),
        _ => None,
    };
    drop(eval);

    match decision {
        PolicyDecision::Deny => {
            debug!(domain = domain, "denying HTTP request");
            send_http_error(&mut stream, 403, "Forbidden by network policy").await?;
        }
        PolicyDecision::Allow | PolicyDecision::AllowWithTransform(_) => {
            // Build modified request with injected headers if needed.
            let upstream_data = if let Some(ref rule) = transform_rules {
                inject_headers(request_data, &rule.headers)?
            } else {
                request_data.to_vec()
            };

            let upstream_port = extract_port(host, 80);
            let upstream_addr = format!("{domain}:{upstream_port}");

            match TcpStream::connect(&upstream_addr).await {
                Ok(mut upstream) => {
                    upstream.write_all(&upstream_data).await?;
                    relay_bidirectional(&mut stream, &mut upstream).await?;
                }
                Err(e) => {
                    warn!(domain = domain, error = %e, "upstream connection failed");
                    send_http_error(&mut stream, 502, "Bad Gateway").await?;
                }
            }
        }
    }

    Ok(())
}

/// Handle HTTPS CONNECT: extract domain, evaluate, optionally MITM.
async fn handle_https_connect(
    mut stream: TcpStream,
    _peer: SocketAddr,
    evaluator: Arc<RwLock<NetworkPolicyEvaluator>>,
    ca: Arc<CertificateAuthority>,
) -> io::Result<()> {
    // Read the CONNECT request line and headers.
    let buf = match read_until_headers_end(&mut stream).await {
        Ok(b) if b.is_empty() => return Ok(()),
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::InvalidData => {
            send_http_error(&mut stream, 431, "Request Header Fields Too Large").await?;
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut headers);
    let _ = req.parse(&buf).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("CONNECT parse: {e}"))
    })?;

    let path = req.path.unwrap_or_default();
    let domain = path.split(':').next().unwrap_or(path);
    let upstream_port = extract_port(path, 443);

    if domain.is_empty() {
        send_http_error(&mut stream, 400, "Bad Request").await?;
        return Ok(());
    }

    // Evaluate policy.
    let ip = resolve_domain(domain).await;
    let eval = evaluator.read().await;

    let decision = match ip {
        Some(addr) => eval.evaluate_connection(domain, addr),
        None => {
            if eval.should_allow_domain(domain) {
                PolicyDecision::Allow
            } else {
                PolicyDecision::Deny
            }
        }
    };
    drop(eval);

    match decision {
        PolicyDecision::Deny => {
            debug!(domain = domain, "denying HTTPS CONNECT");
            send_http_error(&mut stream, 403, "Forbidden by network policy").await?;
            return Ok(());
        }
        PolicyDecision::AllowWithTransform(rule) => {
            // MITM: TLS handshake with client, connect to upstream, inject headers.
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;

            let upstream_addr = format!("{domain}:{upstream_port}");
            let upstream = TcpStream::connect(&upstream_addr).await.map_err(|e| {
                io::Error::new(io::ErrorKind::ConnectionRefused, format!("upstream: {e}"))
            })?;

            // Issue a cert for this domain and set up TLS with the client.
            let (cert_pem, key_pem) = ca.issue_cert(domain).map_err(|e| {
                io::Error::other(format!("cert issue: {e}"))
            })?;

            let certs = rustls_pemfile::certs(&mut cert_pem.as_bytes())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("parse cert: {e}")))?;

            let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("parse key: {e}")))?
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no private key found"))?;

            let tls_config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("tls config: {e}")))?;

            let tls_acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config));
            let mut tls_stream = tls_acceptor.accept(stream).await?;

            // Read the actual HTTP request from the TLS stream (up to 64KB headers).
            let mut inner_buf = Vec::with_capacity(8192);
            let mut tmp = [0u8; 4096];
            loop {
                let n = tls_stream.read(&mut tmp).await?;
                if n == 0 {
                    if inner_buf.is_empty() {
                        return Ok(());
                    }
                    break;
                }
                inner_buf.extend_from_slice(&tmp[..n]);
                if inner_buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if inner_buf.len() > MAX_HEADER_SIZE {
                    // Can't easily send HTTP error over TLS stream, just close.
                    return Ok(());
                }
            }

            // Inject headers into the inner request.
            let modified = inject_headers(&inner_buf, &rule.headers)?;

            // Set up TLS to upstream. Since this is a MITM proxy, we use a
            // permissive verifier — the proxy itself is the trust boundary.
            let upstream_tls_config = rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify))
                .with_no_client_auth();

            let server_name = rustls::pki_types::ServerName::try_from(domain.to_string())
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("server name: {e}")))?;

            let connector = tokio_rustls::TlsConnector::from(Arc::new(upstream_tls_config));
            let mut upstream_tls = connector.connect(server_name, upstream).await?;

            upstream_tls.write_all(&modified).await?;

            // Relay.
            let (mut cr, mut cw) = tokio::io::split(tls_stream);
            let (mut ur, mut uw) = tokio::io::split(upstream_tls);

            let c2u = tokio::io::copy(&mut cr, &mut uw);
            let u2c = tokio::io::copy(&mut ur, &mut cw);
            let _ = tokio::try_join!(c2u, u2c);
        }
        PolicyDecision::Allow => {
            // Simple tunnel — no MITM needed.
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;

            let upstream_addr = format!("{domain}:{upstream_port}");
            match TcpStream::connect(&upstream_addr).await {
                Ok(mut upstream) => {
                    relay_bidirectional(&mut stream, &mut upstream).await?;
                }
                Err(e) => {
                    warn!(domain = domain, error = %e, "upstream CONNECT failed");
                    // Connection already established to client, just close.
                }
            }
        }
    }

    Ok(())
}

/// Extract the Host header value from a parsed request.
fn extract_host<'a>(req: &httparse::Request<'a, '_>) -> Option<&'a str> {
    for header in req.headers.iter() {
        if header.name.eq_ignore_ascii_case("host") {
            return std::str::from_utf8(header.value).ok();
        }
    }
    None
}

/// Extract port from a host:port string, defaulting to `default_port`.
fn extract_port(host: &str, default_port: u16) -> u16 {
    host.rsplit_once(':')
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or(default_port)
}

/// Resolve a domain to an IP address via DNS.
async fn resolve_domain(domain: &str) -> Option<std::net::IpAddr> {
    tokio::net::lookup_host(format!("{domain}:0"))
        .await
        .ok()
        .and_then(|mut addrs| addrs.next())
        .map(|sa| sa.ip())
}

/// Inject headers into raw HTTP request bytes, returning modified request.
pub fn inject_headers(
    request: &[u8],
    headers: &std::collections::HashMap<String, String>,
) -> io::Result<Vec<u8>> {
    if headers.is_empty() {
        return Ok(request.to_vec());
    }

    // Find the end of headers (\r\n\r\n).
    let header_end = request
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no header terminator"))?;

    let mut result = Vec::with_capacity(request.len() + headers.len() * 64);
    result.extend_from_slice(&request[..header_end + 2]); // up to last \r\n before the empty line

    // Append injected headers.
    for (key, value) in headers {
        result.extend_from_slice(key.as_bytes());
        result.extend_from_slice(b": ");
        result.extend_from_slice(value.as_bytes());
        result.extend_from_slice(b"\r\n");
    }

    // Append the rest (the \r\n and body).
    result.extend_from_slice(&request[header_end + 2..]);
    Ok(result)
}

/// Send an HTTP error response.
async fn send_http_error(
    stream: &mut TcpStream,
    status: u16,
    message: &str,
) -> io::Result<()> {
    let reason = match status {
        400 => "Bad Request",
        403 => "Forbidden",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        _ => "Error",
    };
    let body = format!(
        "<html><body><h1>{status} {reason}</h1><p>{message}</p></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

/// Bidirectional relay between two TCP streams.
async fn relay_bidirectional(
    a: &mut TcpStream,
    b: &mut TcpStream,
) -> io::Result<()> {
    let (mut ar, mut aw) = a.split();
    let (mut br, mut bw) = b.split();

    let a2b = tokio::io::copy(&mut ar, &mut bw);
    let b2a = tokio::io::copy(&mut br, &mut aw);

    let _ = tokio::try_join!(a2b, b2a);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn extract_host_from_request() {
        let raw = b"GET / HTTP/1.1\r\nHost: example.com\r\nAccept: */*\r\n\r\n";
        let mut headers = [httparse::EMPTY_HEADER; 16];
        let mut req = httparse::Request::new(&mut headers);
        req.parse(raw).unwrap();
        assert_eq!(extract_host(&req), Some("example.com"));
    }

    #[test]
    fn extract_host_missing() {
        let raw = b"GET / HTTP/1.1\r\nAccept: */*\r\n\r\n";
        let mut headers = [httparse::EMPTY_HEADER; 16];
        let mut req = httparse::Request::new(&mut headers);
        req.parse(raw).unwrap();
        assert_eq!(extract_host(&req), None);
    }

    #[test]
    fn extract_port_with_port() {
        assert_eq!(extract_port("example.com:8080", 80), 8080);
    }

    #[test]
    fn extract_port_without_port() {
        assert_eq!(extract_port("example.com", 80), 80);
    }

    #[test]
    fn inject_headers_adds_to_request() {
        let raw = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer tok123".to_string());

        let result = inject_headers(raw, &headers).unwrap();
        let result_str = String::from_utf8(result).unwrap();

        assert!(result_str.contains("Authorization: Bearer tok123\r\n"));
        assert!(result_str.contains("Host: example.com\r\n"));
        assert!(result_str.ends_with("\r\n\r\n"));
    }

    #[test]
    fn inject_headers_empty_is_noop() {
        let raw = b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let headers = HashMap::new();
        let result = inject_headers(raw, &headers).unwrap();
        assert_eq!(result, raw);
    }

    #[test]
    fn inject_headers_preserves_body() {
        let raw = b"POST / HTTP/1.1\r\nHost: example.com\r\nContent-Length: 5\r\n\r\nhello";
        let mut headers = HashMap::new();
        headers.insert("X-Key".to_string(), "val".to_string());

        let result = inject_headers(raw, &headers).unwrap();
        let result_str = String::from_utf8(result).unwrap();
        assert!(result_str.ends_with("\r\n\r\nhello"));
        assert!(result_str.contains("X-Key: val\r\n"));
    }

    #[test]
    fn needs_domain_proxy_with_allow_domains() {
        use rustbox_core::network::{NetworkMode, NetworkPolicy};

        let policy = NetworkPolicy {
            mode: NetworkMode::DenyAll,
            allow_domains: vec!["*.github.com".to_string()],
            ..NetworkPolicy::default()
        };
        assert!(needs_domain_proxy(&policy));
    }

    #[test]
    fn needs_domain_proxy_with_transform_rules() {
        use rustbox_core::network::{NetworkPolicy, TransformRule};

        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer tok".to_string());
        let policy = NetworkPolicy {
            transform_rules: vec![TransformRule {
                domain: "api.example.com".to_string(),
                headers,
            }],
            ..NetworkPolicy::default()
        };
        assert!(needs_domain_proxy(&policy));
    }

    #[test]
    fn no_proxy_for_subnet_only_deny_all() {
        use rustbox_core::network::{NetworkMode, NetworkPolicy};

        let policy = NetworkPolicy {
            mode: NetworkMode::DenyAll,
            subnets_allow: vec!["10.0.0.0/8".parse().unwrap()],
            ..NetworkPolicy::default()
        };
        assert!(!needs_domain_proxy(&policy));
    }

    #[test]
    fn no_proxy_for_default_policy() {
        let policy = NetworkPolicy::default();
        assert!(!needs_domain_proxy(&policy));
    }

    #[tokio::test]
    async fn proxy_denies_blocked_domain() {
        use rustbox_core::network::{NetworkMode, NetworkPolicy};

        let policy = NetworkPolicy {
            mode: NetworkMode::DenyAll,
            allow_domains: vec!["allowed.example.com".to_string()],
            ..NetworkPolicy::default()
        };
        let ca = crate::tls_proxy::CertificateAuthority::generate().unwrap();
        let proxy = TransparentProxy::start(policy, ca).await.unwrap();
        let port = proxy.port();

        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: blocked.example.com\r\n\r\n")
            .await
            .unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(
            response.starts_with("HTTP/1.1 403"),
            "expected 403 Forbidden, got: {response}"
        );
    }

    #[tokio::test]
    async fn proxy_allows_permitted_domain() {
        use rustbox_core::network::{NetworkMode, NetworkPolicy};

        let policy = NetworkPolicy {
            mode: NetworkMode::DenyAll,
            allow_domains: vec!["allowed.example.com".to_string()],
            ..NetworkPolicy::default()
        };
        let ca = crate::tls_proxy::CertificateAuthority::generate().unwrap();
        let proxy = TransparentProxy::start(policy, ca).await.unwrap();
        let port = proxy.port();

        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        // Send request to allowed domain — proxy should try upstream and fail with 502
        // since there's no actual server at allowed.example.com.
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: allowed.example.com\r\n\r\n")
            .await
            .unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        let response = String::from_utf8_lossy(&buf[..n]);
        // Should NOT be 403 — expect 502 (upstream unreachable) or connection behavior
        assert!(
            !response.starts_with("HTTP/1.1 403"),
            "allowed domain should not be denied, got: {response}"
        );
    }

    #[tokio::test]
    async fn proxy_injects_transform_headers() {
        use rustbox_core::network::{NetworkPolicy, TransformRule};

        // Start a mock upstream server.
        let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock_addr = mock_listener.local_addr().unwrap();

        let mock_handle = tokio::spawn(async move {
            let (mut stream, _) = mock_listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap();
            let received = String::from_utf8_lossy(&buf[..n]).to_string();
            // Send a minimal response so the proxy doesn't hang.
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            received
        });

        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer tok123".to_string());
        let policy = NetworkPolicy {
            transform_rules: vec![TransformRule {
                domain: "127.0.0.1".to_string(),
                headers,
            }],
            ..NetworkPolicy::default()
        };

        let ca = crate::tls_proxy::CertificateAuthority::generate().unwrap();
        let proxy = TransparentProxy::start(policy, ca).await.unwrap();
        let proxy_port = proxy.port();

        let mut stream = TcpStream::connect(format!("127.0.0.1:{proxy_port}"))
            .await
            .unwrap();
        let request = format!(
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n",
            mock_addr.port()
        );
        stream.write_all(request.as_bytes()).await.unwrap();

        // Read the response from proxy (forwarded from mock).
        let mut buf = vec![0u8; 4096];
        let _ = stream.read(&mut buf).await.unwrap();

        // Check what the mock upstream received.
        let received = mock_handle.await.unwrap();
        assert!(
            received.contains("Authorization: Bearer tok123"),
            "expected injected header in upstream request, got: {received}"
        );
    }

    #[tokio::test]
    async fn proxy_handles_connect_deny() {
        use rustbox_core::network::{NetworkMode, NetworkPolicy};

        let policy = NetworkPolicy {
            mode: NetworkMode::DenyAll,
            allow_domains: vec![],
            ..NetworkPolicy::default()
        };
        let ca = crate::tls_proxy::CertificateAuthority::generate().unwrap();
        let proxy = TransparentProxy::start(policy, ca).await.unwrap();
        let port = proxy.port();

        let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
            .await
            .unwrap();
        stream
            .write_all(b"CONNECT blocked.example.com:443 HTTP/1.1\r\nHost: blocked.example.com:443\r\n\r\n")
            .await
            .unwrap();

        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        let response = String::from_utf8_lossy(&buf[..n]);
        assert!(
            response.starts_with("HTTP/1.1 403"),
            "expected 403 for blocked CONNECT, got: {response}"
        );
    }

    #[tokio::test]
    async fn update_policy_swaps_evaluator() {
        use rustbox_core::network::{NetworkMode, NetworkPolicy};

        // Use a non-resolving domain to avoid IP-level policy checks.
        // Start with a policy that allows nxdomain.test.invalid.
        let policy = NetworkPolicy {
            mode: NetworkMode::DenyAll,
            allow_domains: vec!["nxdomain.test.invalid".to_string()],
            ..NetworkPolicy::default()
        };
        let ca = crate::tls_proxy::CertificateAuthority::generate().unwrap();
        let proxy = TransparentProxy::start(policy, ca).await.unwrap();
        let port = proxy.port();

        // Verify nxdomain.test.invalid is allowed (should get 502 not 403,
        // since domain is allowed but can't resolve).
        {
            let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
                .await
                .unwrap();
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: nxdomain.test.invalid\r\n\r\n")
                .await
                .unwrap();
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let response = String::from_utf8_lossy(&buf[..n]);
            assert!(
                !response.starts_with("HTTP/1.1 403"),
                "nxdomain.test.invalid should be allowed initially, got: {response}"
            );
        }

        // Update policy to deny everything.
        let new_policy = NetworkPolicy {
            mode: NetworkMode::DenyAll,
            allow_domains: vec![],
            ..NetworkPolicy::default()
        };
        proxy.update_policy(new_policy).await;

        // Small delay to let the policy swap propagate.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        // Now the domain should be denied.
        {
            let mut stream = TcpStream::connect(format!("127.0.0.1:{port}"))
                .await
                .unwrap();
            stream
                .write_all(b"GET / HTTP/1.1\r\nHost: nxdomain.test.invalid\r\n\r\n")
                .await
                .unwrap();
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).await.unwrap();
            let response = String::from_utf8_lossy(&buf[..n]);
            assert!(
                response.starts_with("HTTP/1.1 403"),
                "nxdomain.test.invalid should be denied after policy update, got: {response}"
            );
        }
    }
}
