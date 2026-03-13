use rustbox_core::sandbox::Runtime;
use rustbox_sdk::RustboxClient;
use rustbox_storage::SnapshotStore;
use rustbox_vm::mock_backend::MockBackend;
use std::sync::Arc;
use tokio::net::TcpListener;

async fn start_test_server() -> (String, tokio::task::JoinHandle<()>) {
    let backend = Arc::new(MockBackend::new());
    let snapshot_store = SnapshotStore::new_in_memory().unwrap();
    let orchestrator = Arc::new(rustbox_daemon::orchestrator::Orchestrator::new(
        backend,
        snapshot_store,
    ));
    let app = rustbox_daemon::build_router(orchestrator);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("http://{addr}");

    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (url, handle)
}

#[tokio::test]
async fn create_sandbox() {
    let (url, _handle) = start_test_server().await;
    let client = RustboxClient::new(&url);
    let sandbox = client.create_sandbox(Runtime::Node24, 300).await.unwrap();
    assert!(!sandbox.id.is_empty());
    // status should be Running (daemon creates + starts)
}

#[tokio::test]
async fn list_sandboxes() {
    let (url, _handle) = start_test_server().await;
    let client = RustboxClient::new(&url);
    client.create_sandbox(Runtime::Node24, 300).await.unwrap();
    client
        .create_sandbox(Runtime::Python313, 300)
        .await
        .unwrap();
    let list = client.list_sandboxes().await.unwrap();
    assert_eq!(list.len(), 2);
}

#[tokio::test]
async fn get_sandbox() {
    let (url, _handle) = start_test_server().await;
    let client = RustboxClient::new(&url);
    let created = client.create_sandbox(Runtime::Node24, 300).await.unwrap();
    let got = client.get_sandbox(&created.id).await.unwrap();
    assert_eq!(got.id, created.id);
}

#[tokio::test]
async fn get_nonexistent_sandbox() {
    let (url, _handle) = start_test_server().await;
    let client = RustboxClient::new(&url);
    let result = client.get_sandbox("nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn delete_sandbox() {
    let (url, _handle) = start_test_server().await;
    let client = RustboxClient::new(&url);
    let sandbox = client.create_sandbox(Runtime::Node24, 300).await.unwrap();
    client.delete_sandbox(&sandbox.id).await.unwrap();
    let result = client.get_sandbox(&sandbox.id).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn exec_and_get_command() {
    let (url, _handle) = start_test_server().await;
    let client = RustboxClient::new(&url);
    let sandbox = client.create_sandbox(Runtime::Node24, 300).await.unwrap();
    let cmd_id = client.exec(&sandbox.id, "echo", &["hello"]).await.unwrap();
    assert!(!cmd_id.is_empty());
    // Give the mock backend time to send output
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let cmd = client.get_command(&sandbox.id, &cmd_id).await.unwrap();
    assert_eq!(cmd.command_id, cmd_id);
}

#[tokio::test]
async fn upload_and_download_file() {
    let (url, _handle) = start_test_server().await;
    let client = RustboxClient::new(&url);
    let sandbox = client.create_sandbox(Runtime::Node24, 300).await.unwrap();
    client
        .upload_file(&sandbox.id, "/tmp/test.txt", b"hello world")
        .await
        .unwrap();
    let content = client
        .download_file(&sandbox.id, "/tmp/test.txt")
        .await
        .unwrap();
    // MockBackend returns b"mock file content" for any read
    assert_eq!(content, b"mock file content");
}

#[tokio::test]
async fn mkdir_test() {
    let (url, _handle) = start_test_server().await;
    let client = RustboxClient::new(&url);
    let sandbox = client.create_sandbox(Runtime::Node24, 300).await.unwrap();
    client.mkdir(&sandbox.id, "/tmp/testdir").await.unwrap();
}

#[tokio::test]
async fn create_snapshot_test() {
    let (url, _handle) = start_test_server().await;
    let client = RustboxClient::new(&url);
    let sandbox = client.create_sandbox(Runtime::Node24, 300).await.unwrap();
    let snap = client
        .create_snapshot(&sandbox.id, Some("test snapshot"))
        .await
        .unwrap();
    assert!(!snap.id.is_empty());
    assert_eq!(snap.sandbox_id, sandbox.id);
}

#[tokio::test]
async fn update_timeout_test() {
    let (url, _handle) = start_test_server().await;
    let client = RustboxClient::new(&url);
    let sandbox = client.create_sandbox(Runtime::Node24, 300).await.unwrap();
    let updated = client.update_timeout(&sandbox.id, 600).await.unwrap();
    assert_eq!(updated.id, sandbox.id);
}

#[tokio::test]
async fn get_metrics_test() {
    let (url, _handle) = start_test_server().await;
    let client = RustboxClient::new(&url);
    let sandbox = client.create_sandbox(Runtime::Node24, 300).await.unwrap();
    let metrics = client.get_metrics(&sandbox.id).await.unwrap();
    assert_eq!(metrics.cpu_usage_percent, 0.0);
}
