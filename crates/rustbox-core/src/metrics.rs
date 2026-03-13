use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SandboxMetrics {
    pub cpu_usage_percent: f64,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub network_rx_bytes: u64,
    pub network_tx_bytes: u64,
    pub disk_used_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json;

    #[test]
    fn default_all_zeros() {
        let m = SandboxMetrics::default();
        assert_eq!(m.cpu_usage_percent, 0.0);
        assert_eq!(m.memory_used_bytes, 0);
        assert_eq!(m.memory_total_bytes, 0);
        assert_eq!(m.network_rx_bytes, 0);
        assert_eq!(m.network_tx_bytes, 0);
        assert_eq!(m.disk_used_bytes, 0);
    }

    #[test]
    fn serde_roundtrip() {
        let m = SandboxMetrics {
            cpu_usage_percent: 45.5,
            memory_used_bytes: 1024 * 1024,
            memory_total_bytes: 4 * 1024 * 1024,
            network_rx_bytes: 500,
            network_tx_bytes: 300,
            disk_used_bytes: 2048,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: SandboxMetrics = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cpu_usage_percent, 45.5);
        assert_eq!(back.memory_used_bytes, 1024 * 1024);
        assert_eq!(back.memory_total_bytes, 4 * 1024 * 1024);
        assert_eq!(back.network_rx_bytes, 500);
        assert_eq!(back.network_tx_bytes, 300);
        assert_eq!(back.disk_used_bytes, 2048);
    }
}
