use rustbox_storage::SnapshotStore;
use std::sync::Arc;
use tracing::{info, warn};

pub struct SnapshotReaper {
    snapshot_store: Arc<SnapshotStore>,
}

impl SnapshotReaper {
    pub fn new(snapshot_store: Arc<SnapshotStore>) -> Self {
        Self { snapshot_store }
    }

    /// Reap expired snapshots once. Returns number deleted.
    pub fn reap_once(&self) -> usize {
        let expired = match self.snapshot_store.list_expired() {
            Ok(list) => list,
            Err(e) => {
                warn!(error = %e, "failed to list expired snapshots");
                return 0;
            }
        };

        let mut deleted = 0;
        for snap in &expired {
            info!(snapshot_id = %snap.id, "reaping expired snapshot");
            match self.snapshot_store.delete(&snap.id) {
                Ok(true) => deleted += 1,
                Ok(false) => {}
                Err(e) => warn!(snapshot_id = %snap.id, error = %e, "failed to delete snapshot"),
            }
        }

        deleted
    }

    /// Run the reaper loop.
    pub async fn run(self, interval: std::time::Duration) {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            self.reap_once();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};
    use rustbox_storage::SnapshotMetadata;

    fn make_store() -> Arc<SnapshotStore> {
        Arc::new(SnapshotStore::new_in_memory().unwrap())
    }

    #[test]
    fn expired_snapshot_gets_deleted() {
        let store = make_store();
        let now = Utc::now();

        let snap = SnapshotMetadata {
            id: "snap-expired".to_string(),
            sandbox_id: "sb-1".to_string(),
            created_at: now - Duration::hours(2),
            expires_at: Some(now - Duration::hours(1)),
            size_bytes: 1024,
            description: Some("old snapshot".to_string()),
        };
        store.save(&snap).unwrap();

        let reaper = SnapshotReaper::new(Arc::clone(&store));
        let deleted = reaper.reap_once();
        assert_eq!(deleted, 1);

        let result = store.get("snap-expired").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn non_expired_survives() {
        let store = make_store();
        let now = Utc::now();

        let snap = SnapshotMetadata {
            id: "snap-fresh".to_string(),
            sandbox_id: "sb-1".to_string(),
            created_at: now,
            expires_at: Some(now + Duration::hours(1)),
            size_bytes: 2048,
            description: Some("fresh snapshot".to_string()),
        };
        store.save(&snap).unwrap();

        let reaper = SnapshotReaper::new(Arc::clone(&store));
        let deleted = reaper.reap_once();
        assert_eq!(deleted, 0);

        let result = store.get("snap-fresh").unwrap();
        assert!(result.is_some());
    }
}
