use rustbox_daemon::build_router;
use rustbox_daemon::orchestrator::Orchestrator;
use rustbox_storage::SnapshotStore;
use rustbox_vm::backend::VmBackend;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let backend: Arc<dyn VmBackend> = match std::env::var("RUSTBOX_BACKEND").as_deref() {
        Ok("local") => {
            tracing::info!("using LocalBackend (no isolation)");
            Arc::new(rustbox_vm::local_backend::LocalBackend::new())
        }
        #[cfg(target_os = "linux")]
        Ok("firecracker") => {
            use rustbox_vm::firecracker::backend::{FirecrackerBackend, FirecrackerBackendConfig};
            tracing::info!("using FirecrackerBackend (Linux)");
            let config = FirecrackerBackendConfig {
                firecracker_bin: std::path::PathBuf::from("firecracker"),
                kernel_path: std::path::PathBuf::from("/opt/rustbox/images/vmlinux"),
                rootfs_dir: std::path::PathBuf::from("/opt/rustbox/images"),
                state_dir: std::path::PathBuf::from("/var/lib/rustbox/state"),
                vsock_base_dir: std::path::PathBuf::from("/var/lib/rustbox/vsock"),
            };
            Arc::new(FirecrackerBackend::new(config))
        }
        _ => {
            use rustbox_vm::docker::backend::{DockerBackend, DockerBackendConfig};
            tracing::info!("using DockerBackend");
            let config = DockerBackendConfig::default();
            Arc::new(
                DockerBackend::new(config)
                    .await
                    .expect("failed to initialize Docker backend"),
            )
        }
    };

    let snapshot_store = SnapshotStore::new("rustbox.db").expect("failed to open snapshot store");
    let orchestrator = Arc::new(Orchestrator::new(backend, snapshot_store));

    let app = build_router(orchestrator);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .unwrap();
    tracing::info!("rustboxd listening on :8080");
    axum::serve(listener, app).await.unwrap();
}
