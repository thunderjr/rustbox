use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::command::{CommandOutput, CommandRequest};
use crate::error::Result;
use crate::id::{CommandId, SandboxId, SnapshotId};
use crate::metrics::SandboxMetrics;
use crate::sandbox::{SandboxConfig, SandboxStatus};

#[async_trait]
pub trait VmBackend: Send + Sync {
    async fn create(&self, id: &SandboxId, config: &SandboxConfig) -> Result<()>;
    async fn start(&self, id: &SandboxId) -> Result<()>;
    async fn stop(&self, id: &SandboxId, blocking: bool) -> Result<()>;
    async fn status(&self, id: &SandboxId) -> Result<SandboxStatus>;
    async fn exec(
        &self,
        id: &SandboxId,
        cmd: &CommandRequest,
    ) -> Result<(CommandId, mpsc::Receiver<CommandOutput>)>;
    async fn kill_command(
        &self,
        id: &SandboxId,
        cmd_id: &CommandId,
        signal: i32,
    ) -> Result<()>;
    async fn write_file(&self, id: &SandboxId, path: &str, content: &[u8]) -> Result<()>;
    async fn read_file(&self, id: &SandboxId, path: &str) -> Result<Vec<u8>>;
    async fn mkdir(&self, id: &SandboxId, path: &str) -> Result<()>;
    async fn snapshot_create(&self, id: &SandboxId) -> Result<SnapshotId>;
    async fn snapshot_restore(&self, id: &SandboxId, snap: &SnapshotId) -> Result<()>;
    async fn metrics(&self, id: &SandboxId) -> Result<SandboxMetrics>;
}
