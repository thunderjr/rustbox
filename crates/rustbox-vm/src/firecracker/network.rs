//! Networking helpers for Firecracker VMs.
//!
//! TAP device management, MAC generation, and nftables rule application.
//! TAP and nftables functions are Linux-only; MAC generation is cross-platform.

#[cfg(target_os = "linux")]
use rustbox_core::{Result, RustboxError};
use rustbox_core::SandboxId;

/// Generate a locally-administered unicast MAC address from a sandbox ID.
///
/// Uses bytes from the UUID string, with the first octet set to `02` (locally
/// administered, unicast).
pub fn generate_mac(id: &SandboxId) -> String {
    // Parse the UUID to get raw bytes, using the random portion (bytes 10-15)
    // to avoid collisions from UUIDv7's timestamp prefix.
    let uuid = uuid::Uuid::parse_str(&id.0).expect("SandboxId should be a valid UUID");
    let b = uuid.as_bytes();
    format!(
        "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        b[10], b[11], b[12], b[13], b[14]
    )
}

/// Create a TAP device with the given name.
#[cfg(target_os = "linux")]
pub fn create_tap(name: &str) -> Result<std::os::unix::io::RawFd> {
    use nix::sys::socket::{socket, AddressFamily, SockFlag, SockType};
    use std::os::unix::io::AsRawFd;

    // Open /dev/net/tun
    let fd = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
        .map_err(|e| RustboxError::VmBackend(format!("open /dev/net/tun: {e}")))?;

    // Set up ifreq for TUNSETIFF ioctl
    let mut ifr = [0u8; 40]; // struct ifreq is 40 bytes
    let name_bytes = name.as_bytes();
    if name_bytes.len() >= 16 {
        return Err(RustboxError::VmBackend(format!(
            "TAP name too long (max 15 chars): {name}"
        )));
    }
    ifr[..name_bytes.len()].copy_from_slice(name_bytes);

    // IFF_TAP | IFF_NO_PI = 0x0002 | 0x1000
    let flags: u16 = 0x0002 | 0x1000;
    ifr[16..18].copy_from_slice(&flags.to_le_bytes());

    // TUNSETIFF = 0x400454ca
    let raw_fd = fd.as_raw_fd();
    unsafe {
        let ret = libc::ioctl(raw_fd, 0x400454ca, ifr.as_ptr());
        if ret < 0 {
            return Err(RustboxError::VmBackend(format!(
                "TUNSETIFF ioctl failed: {}",
                std::io::Error::last_os_error()
            )));
        }
    }

    // Bring the interface up using a socket + SIOCSIFFLAGS
    let sock = socket(
        AddressFamily::Inet,
        SockType::Datagram,
        SockFlag::empty(),
        None,
    )
    .map_err(|e| RustboxError::VmBackend(format!("create socket: {e}")))?;

    // Read current flags with SIOCGIFFLAGS
    let mut ifr_flags = [0u8; 40];
    ifr_flags[..name_bytes.len()].copy_from_slice(name_bytes);
    unsafe {
        let ret = libc::ioctl(
            sock.as_raw_fd(),
            libc::SIOCGIFFLAGS as libc::c_ulong,
            ifr_flags.as_ptr(),
        );
        if ret < 0 {
            return Err(RustboxError::VmBackend(format!(
                "SIOCGIFFLAGS ioctl failed: {}",
                std::io::Error::last_os_error()
            )));
        }
    }

    // Set IFF_UP
    let mut current_flags = u16::from_le_bytes([ifr_flags[16], ifr_flags[17]]);
    current_flags |= libc::IFF_UP as u16;
    ifr_flags[16..18].copy_from_slice(&current_flags.to_le_bytes());
    unsafe {
        let ret = libc::ioctl(
            sock.as_raw_fd(),
            libc::SIOCSIFFLAGS as libc::c_ulong,
            ifr_flags.as_ptr(),
        );
        if ret < 0 {
            return Err(RustboxError::VmBackend(format!(
                "SIOCSIFFLAGS ioctl failed: {}",
                std::io::Error::last_os_error()
            )));
        }
    }

    // Leak the fd so it stays open (Firecracker needs it)
    let raw = fd.as_raw_fd();
    std::mem::forget(fd);
    Ok(raw)
}

/// Delete a TAP device by bringing the interface down.
#[cfg(target_os = "linux")]
pub fn delete_tap(name: &str) -> Result<()> {
    use nix::sys::socket::{socket, AddressFamily, SockFlag, SockType};
    use std::os::unix::io::AsRawFd;

    let sock = socket(
        AddressFamily::Inet,
        SockType::Datagram,
        SockFlag::empty(),
        None,
    )
    .map_err(|e| RustboxError::VmBackend(format!("create socket: {e}")))?;

    let name_bytes = name.as_bytes();
    let mut ifr = [0u8; 40];
    ifr[..name_bytes.len()].copy_from_slice(name_bytes);

    // Bring interface down
    unsafe {
        let ret = libc::ioctl(
            sock.as_raw_fd(),
            libc::SIOCGIFFLAGS as libc::c_ulong,
            ifr.as_ptr(),
        );
        if ret >= 0 {
            let mut flags = u16::from_le_bytes([ifr[16], ifr[17]]);
            flags &= !(libc::IFF_UP as u16);
            ifr[16..18].copy_from_slice(&flags.to_le_bytes());
            libc::ioctl(
                sock.as_raw_fd(),
                libc::SIOCSIFFLAGS as libc::c_ulong,
                ifr.as_ptr(),
            );
        }
    }

    Ok(())
}

/// Apply nftables rules for a sandbox by creating a per-sandbox table.
#[cfg(target_os = "linux")]
pub async fn apply_nftables(
    table_name: &str,
    rules: &rustbox_network::NftablesRuleSet,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    // Build nft script with per-sandbox table
    let mut nft_commands = vec![
        format!("add table inet {table_name}"),
        format!(
            "add chain inet {table_name} output {{ type filter hook output priority 0; policy accept; }}"
        ),
    ];

    // Add rules, scoped to this table
    for rule in &rules.rules {
        let scoped_rule =
            rule.replace("inet filter output", &format!("inet {table_name} output"));
        nft_commands.push(scoped_rule);
    }

    let nft_script = nft_commands.join("\n");

    let mut child = tokio::process::Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| RustboxError::VmBackend(format!("spawn nft: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(nft_script.as_bytes()).await.map_err(|e| {
            RustboxError::VmBackend(format!("write nft rules: {e}"))
        })?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| RustboxError::VmBackend(format!("nft wait: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RustboxError::VmBackend(format!("nft failed: {stderr}")));
    }

    Ok(())
}

/// Remove nftables rules for a sandbox by deleting the per-sandbox table.
#[cfg(target_os = "linux")]
pub async fn remove_nftables(table_name: &str) -> Result<()> {
    let output = tokio::process::Command::new("nft")
        .args(["delete", "table", "inet", table_name])
        .output()
        .await
        .map_err(|e| RustboxError::VmBackend(format!("spawn nft delete: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RustboxError::VmBackend(format!(
            "nft delete table failed: {stderr}"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_mac_is_locally_administered() {
        let id = SandboxId::new();
        let mac = generate_mac(&id);

        // Should start with 02: (locally administered, unicast)
        assert!(
            mac.starts_with("02:"),
            "MAC should start with 02:, got: {mac}"
        );

        // Should be valid MAC format XX:XX:XX:XX:XX:XX
        let parts: Vec<&str> = mac.split(':').collect();
        assert_eq!(parts.len(), 6, "MAC should have 6 octets, got: {mac}");

        for part in &parts {
            assert_eq!(part.len(), 2, "each octet should be 2 hex chars: {part}");
            assert!(
                u8::from_str_radix(part, 16).is_ok(),
                "each octet should be valid hex: {part}"
            );
        }
    }

    #[test]
    fn generate_mac_deterministic() {
        let id = SandboxId::new();
        let mac1 = generate_mac(&id);
        let mac2 = generate_mac(&id);
        assert_eq!(mac1, mac2, "same ID should produce same MAC");
    }

    #[test]
    fn generate_mac_different_ids_differ() {
        let id1 = SandboxId::new();
        let id2 = SandboxId::new();
        let mac1 = generate_mac(&id1);
        let mac2 = generate_mac(&id2);
        assert_ne!(mac1, mac2, "different IDs should produce different MACs");
    }
}
