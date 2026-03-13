use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

use crate::error::{Result, StorageError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotMetadata {
    pub id: String,
    pub sandbox_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub size_bytes: u64,
    pub description: Option<String>,
}

/// SQLite-backed snapshot metadata store.
pub struct SnapshotStore {
    conn: Mutex<Connection>,
}

impl SnapshotStore {
    /// Create a new store backed by a SQLite file.
    pub fn new(path: &str) -> Result<Self> {
        let conn = Connection::open(path)
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_tables()?;
        Ok(store)
    }

    /// Create an in-memory store (for testing).
    pub fn new_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| StorageError::Database(e.to_string()))?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_tables()?;
        Ok(store)
    }

    fn init_tables(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS snapshots (
                id TEXT PRIMARY KEY,
                sandbox_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT,
                size_bytes INTEGER NOT NULL DEFAULT 0,
                description TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_snapshots_sandbox ON snapshots(sandbox_id);
            CREATE INDEX IF NOT EXISTS idx_snapshots_expires ON snapshots(expires_at);
        ",
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn save(&self, metadata: &SnapshotMetadata) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO snapshots (id, sandbox_id, created_at, expires_at, size_bytes, description)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                metadata.id,
                metadata.sandbox_id,
                metadata.created_at.to_rfc3339(),
                metadata.expires_at.map(|t| t.to_rfc3339()),
                metadata.size_bytes,
                metadata.description,
            ],
        )
        .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<SnapshotMetadata>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, sandbox_id, created_at, expires_at, size_bytes, description FROM snapshots WHERE id = ?1",
            )
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let result = stmt.query_row(rusqlite::params![id], |row| {
            Ok(SnapshotMetadata {
                id: row.get(0)?,
                sandbox_id: row.get(1)?,
                created_at: {
                    let s: String = row.get(2)?;
                    DateTime::parse_from_rfc3339(&s)
                        .unwrap()
                        .with_timezone(&Utc)
                },
                expires_at: {
                    let s: Option<String> = row.get(3)?;
                    s.map(|s| {
                        DateTime::parse_from_rfc3339(&s)
                            .unwrap()
                            .with_timezone(&Utc)
                    })
                },
                size_bytes: row.get::<_, i64>(4)? as u64,
                description: row.get(5)?,
            })
        });

        match result {
            Ok(m) => Ok(Some(m)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StorageError::Database(e.to_string())),
        }
    }

    pub fn list_for_sandbox(&self, sandbox_id: &str) -> Result<Vec<SnapshotMetadata>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, sandbox_id, created_at, expires_at, size_bytes, description FROM snapshots WHERE sandbox_id = ?1 ORDER BY created_at DESC",
            )
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(rusqlite::params![sandbox_id], |row| {
                Ok(SnapshotMetadata {
                    id: row.get(0)?,
                    sandbox_id: row.get(1)?,
                    created_at: {
                        let s: String = row.get(2)?;
                        DateTime::parse_from_rfc3339(&s)
                            .unwrap()
                            .with_timezone(&Utc)
                    },
                    expires_at: {
                        let s: Option<String> = row.get(3)?;
                        s.map(|s| {
                            DateTime::parse_from_rfc3339(&s)
                                .unwrap()
                                .with_timezone(&Utc)
                        })
                    },
                    size_bytes: row.get::<_, i64>(4)? as u64,
                    description: row.get(5)?,
                })
            })
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| StorageError::Database(e.to_string()))?);
        }
        Ok(results)
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let affected = conn
            .execute(
                "DELETE FROM snapshots WHERE id = ?1",
                rusqlite::params![id],
            )
            .map_err(|e| StorageError::Database(e.to_string()))?;
        Ok(affected > 0)
    }

    /// List snapshots that have expired (expires_at < now).
    pub fn list_expired(&self) -> Result<Vec<SnapshotMetadata>> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT id, sandbox_id, created_at, expires_at, size_bytes, description FROM snapshots WHERE expires_at IS NOT NULL AND expires_at < ?1",
            )
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let rows = stmt
            .query_map(rusqlite::params![now], |row| {
                Ok(SnapshotMetadata {
                    id: row.get(0)?,
                    sandbox_id: row.get(1)?,
                    created_at: {
                        let s: String = row.get(2)?;
                        DateTime::parse_from_rfc3339(&s)
                            .unwrap()
                            .with_timezone(&Utc)
                    },
                    expires_at: {
                        let s: Option<String> = row.get(3)?;
                        s.map(|s| {
                            DateTime::parse_from_rfc3339(&s)
                                .unwrap()
                                .with_timezone(&Utc)
                        })
                    },
                    size_bytes: row.get::<_, i64>(4)? as u64,
                    description: row.get(5)?,
                })
            })
            .map_err(|e| StorageError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| StorageError::Database(e.to_string()))?);
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_snapshot(id: &str, sandbox_id: &str, expired: bool) -> SnapshotMetadata {
        let now = Utc::now();
        SnapshotMetadata {
            id: id.to_string(),
            sandbox_id: sandbox_id.to_string(),
            created_at: now,
            expires_at: if expired {
                Some(now - Duration::hours(1))
            } else {
                Some(now + Duration::hours(1))
            },
            size_bytes: 1024,
            description: Some(format!("snapshot {id}")),
        }
    }

    #[test]
    fn save_and_get_roundtrip() {
        let store = SnapshotStore::new_in_memory().unwrap();
        let snap = make_snapshot("snap-1", "sb-1", false);
        store.save(&snap).unwrap();

        let loaded = store.get("snap-1").unwrap().expect("should exist");
        assert_eq!(loaded.id, "snap-1");
        assert_eq!(loaded.sandbox_id, "sb-1");
        assert_eq!(loaded.size_bytes, 1024);
        assert_eq!(loaded.description, Some("snapshot snap-1".to_string()));
    }

    #[test]
    fn list_for_sandbox_filtering() {
        let store = SnapshotStore::new_in_memory().unwrap();
        let snap_a1 = make_snapshot("a1", "sandbox-a", false);
        let snap_a2 = make_snapshot("a2", "sandbox-a", false);
        let snap_b1 = make_snapshot("b1", "sandbox-b", false);

        store.save(&snap_a1).unwrap();
        store.save(&snap_a2).unwrap();
        store.save(&snap_b1).unwrap();

        let list_a = store.list_for_sandbox("sandbox-a").unwrap();
        assert_eq!(list_a.len(), 2);
        assert!(list_a.iter().all(|s| s.sandbox_id == "sandbox-a"));

        let list_b = store.list_for_sandbox("sandbox-b").unwrap();
        assert_eq!(list_b.len(), 1);
        assert_eq!(list_b[0].id, "b1");
    }

    #[test]
    fn delete_then_get_returns_none() {
        let store = SnapshotStore::new_in_memory().unwrap();
        let snap = make_snapshot("del-1", "sb-1", false);
        store.save(&snap).unwrap();

        let deleted = store.delete("del-1").unwrap();
        assert!(deleted);

        let loaded = store.get("del-1").unwrap();
        assert!(loaded.is_none());

        // deleting again returns false
        let deleted_again = store.delete("del-1").unwrap();
        assert!(!deleted_again);
    }

    #[test]
    fn list_expired_correctness() {
        let store = SnapshotStore::new_in_memory().unwrap();
        let expired = make_snapshot("exp-1", "sb-1", true);
        let not_expired = make_snapshot("fresh-1", "sb-1", false);

        store.save(&expired).unwrap();
        store.save(&not_expired).unwrap();

        let expired_list = store.list_expired().unwrap();
        assert_eq!(expired_list.len(), 1);
        assert_eq!(expired_list[0].id, "exp-1");
    }

    #[test]
    fn serde_roundtrip() {
        let snap = make_snapshot("serde-1", "sb-1", false);
        let json = serde_json::to_string(&snap).unwrap();
        let back: SnapshotMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }
}
