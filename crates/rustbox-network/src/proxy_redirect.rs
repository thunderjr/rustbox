//! iptables NAT REDIRECT rules for transparent proxy.
//!
//! Routes sandbox traffic on ports 80 and 443 through the transparent proxy.
//! Linux-only — gated behind `#[cfg(target_os = "linux")]` in lib.rs.

use std::io;

/// Set up iptables NAT REDIRECT rules for the given interface to route
/// HTTP (80) and HTTPS (443) traffic through the proxy.
pub async fn setup_redirect(interface: &str, proxy_port: u16) -> io::Result<()> {
    // Redirect outbound port 80 to the proxy.
    run_iptables_nat(&[
        "-t", "nat",
        "-A", "PREROUTING",
        "-i", interface,
        "-p", "tcp",
        "--dport", "80",
        "-j", "REDIRECT",
        "--to-port", &proxy_port.to_string(),
    ])
    .await?;

    // Redirect outbound port 443 to the proxy.
    run_iptables_nat(&[
        "-t", "nat",
        "-A", "PREROUTING",
        "-i", interface,
        "-p", "tcp",
        "--dport", "443",
        "-j", "REDIRECT",
        "--to-port", &proxy_port.to_string(),
    ])
    .await?;

    tracing::info!(
        interface = interface,
        proxy_port = proxy_port,
        "proxy redirect rules installed"
    );

    Ok(())
}

/// Remove iptables NAT REDIRECT rules for the given interface.
pub async fn remove_redirect(interface: &str) -> io::Result<()> {
    // List nat PREROUTING rules and remove those referencing our interface.
    let output = tokio::process::Command::new("iptables")
        .args(["-t", "nat", "-S", "PREROUTING"])
        .output()
        .await?;

    if !output.status.success() {
        return Ok(());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains(interface) && line.contains("REDIRECT") {
            if let Some(rule_part) = line.strip_prefix("-A ") {
                let args: Vec<&str> = rule_part.split_whitespace().collect();
                let mut delete_args = vec!["-t", "nat", "-D"];
                delete_args.extend(&args);
                let _ = run_iptables_nat(&delete_args).await;
            }
        }
    }

    tracing::info!(interface = interface, "proxy redirect rules removed");
    Ok(())
}

async fn run_iptables_nat(args: &[&str]) -> io::Result<()> {
    let output = tokio::process::Command::new("iptables")
        .args(args)
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("iptables {} failed: {stderr}", args.join(" ")),
        ));
    }

    Ok(())
}
