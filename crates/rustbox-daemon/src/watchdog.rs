use crate::orchestrator::Orchestrator;
use chrono::Utc;
use std::sync::Arc;
use tracing::{info, warn};

pub struct TimeoutWatchdog {
    orchestrator: Arc<Orchestrator>,
}

impl TimeoutWatchdog {
    pub fn new(orchestrator: Arc<Orchestrator>) -> Self {
        Self { orchestrator }
    }

    /// Check all sandboxes once and stop any that have exceeded their timeout.
    /// Returns the number of sandboxes stopped.
    pub async fn check_once(&self) -> usize {
        let sandboxes = self.orchestrator.list_sandboxes().await;
        let now = Utc::now();
        let mut stopped = 0;

        for sandbox in sandboxes {
            let elapsed = now.signed_duration_since(sandbox.created_at);
            let timeout_secs = sandbox.config.timeout.as_secs() as i64;

            if elapsed.num_seconds() > timeout_secs {
                let id = sandbox.id.to_string();
                info!(sandbox_id = %id, "sandbox timed out, stopping");
                match self.orchestrator.delete_sandbox(&id).await {
                    Ok(()) => stopped += 1,
                    Err(e) => {
                        warn!(sandbox_id = %id, error = %e, "failed to stop timed-out sandbox")
                    }
                }
            }
        }

        stopped
    }

    /// Run the watchdog loop, checking every `interval`.
    pub async fn run(self, interval: std::time::Duration) {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            self.check_once().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustbox_core::network::NetworkPolicy;
    use rustbox_core::sandbox::{CpuCount, Runtime};
    use rustbox_core::SandboxConfig;
    use rustbox_storage::SnapshotStore;
    use rustbox_vm::mock_backend::MockBackend;
    use std::collections::HashMap;
    use std::time::Duration;

    fn make_orchestrator() -> Arc<Orchestrator> {
        let backend = Arc::new(MockBackend::new());
        let store = SnapshotStore::new_in_memory().unwrap();
        Arc::new(Orchestrator::new(backend, store))
    }

    fn config_with_timeout(secs: u64) -> SandboxConfig {
        SandboxConfig {
            runtime: Runtime::Node24,
            cpu_count: CpuCount::One,
            timeout: Duration::from_secs(secs),
            env: HashMap::new(),
            ports: vec![],
            network_policy: NetworkPolicy::default(),
            source: None,
        }
    }

    #[tokio::test]
    async fn expired_sandbox_gets_stopped() {
        let orch = make_orchestrator();
        let _sandbox = orch.create_sandbox(config_with_timeout(1)).await.unwrap();

        // Wait long enough that num_seconds() > 1 (need >2s elapsed)
        tokio::time::sleep(Duration::from_millis(2100)).await;

        let watchdog = TimeoutWatchdog::new(Arc::clone(&orch));
        let stopped = watchdog.check_once().await;
        assert_eq!(stopped, 1);

        let remaining = orch.list_sandboxes().await;
        assert!(remaining.is_empty());
    }

    #[tokio::test]
    async fn active_sandbox_survives() {
        let orch = make_orchestrator();
        let _sandbox = orch.create_sandbox(config_with_timeout(300)).await.unwrap();

        let watchdog = TimeoutWatchdog::new(Arc::clone(&orch));
        let stopped = watchdog.check_once().await;
        assert_eq!(stopped, 0);

        let remaining = orch.list_sandboxes().await;
        assert_eq!(remaining.len(), 1);
    }
}
