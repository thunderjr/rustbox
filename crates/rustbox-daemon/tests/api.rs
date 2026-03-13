use axum::body::Body;
use axum::http::{Request, StatusCode};
use rustbox_daemon::build_router;
use rustbox_daemon::orchestrator::Orchestrator;
use rustbox_storage::SnapshotStore;
use rustbox_vm::mock_backend::MockBackend;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt; // for oneshot()

fn build_test_app() -> axum::Router {
    let backend = Arc::new(MockBackend::new());
    let snapshot_store = SnapshotStore::new_in_memory().unwrap();
    let orchestrator = Arc::new(Orchestrator::new(backend, snapshot_store));
    build_router(orchestrator)
}

async fn json_request(
    app: axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);

    let body = if let Some(v) = body {
        builder = builder.header("content-type", "application/json");
        Body::from(serde_json::to_vec(&v).unwrap())
    } else {
        Body::empty()
    };

    let req = builder.body(body).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let body_bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body_bytes).unwrap_or(json!(null));
    (status, value)
}

async fn create_sandbox(app: axum::Router) -> (StatusCode, Value) {
    json_request(
        app,
        "POST",
        "/v1/sandboxes",
        Some(json!({"runtime": "node24"})),
    )
    .await
}

#[tokio::test]
async fn test_create_sandbox_201() {
    let app = build_test_app();
    let (status, body) = create_sandbox(app).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["id"].is_string());
    assert_eq!(body["status"], "running");
    assert_eq!(body["runtime"], "node24");
}

#[tokio::test]
async fn test_list_sandboxes() {
    let app = build_test_app();

    // Create one sandbox first
    let (_, created) = create_sandbox(app.clone()).await;
    let _id = created["id"].as_str().unwrap();

    // List
    let (status, body) = json_request(app, "GET", "/v1/sandboxes", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array());
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn test_get_sandbox_200() {
    let app = build_test_app();
    let (_, created) = create_sandbox(app.clone()).await;
    let id = created["id"].as_str().unwrap();

    let (status, body) = json_request(app, "GET", &format!("/v1/sandboxes/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], id);
}

#[tokio::test]
async fn test_get_nonexistent_404() {
    let app = build_test_app();
    let (status, body) = json_request(app, "GET", "/v1/sandboxes/fake-id", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn test_delete_sandbox() {
    let app = build_test_app();
    let (_, created) = create_sandbox(app.clone()).await;
    let id = created["id"].as_str().unwrap();

    let (status, _) =
        json_request(app.clone(), "DELETE", &format!("/v1/sandboxes/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);

    // Verify deleted
    let (status, _) = json_request(app, "GET", &format!("/v1/sandboxes/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_update_timeout() {
    let app = build_test_app();
    let (_, created) = create_sandbox(app.clone()).await;
    let id = created["id"].as_str().unwrap();

    let (status, body) = json_request(
        app,
        "PATCH",
        &format!("/v1/sandboxes/{id}/timeout"),
        Some(json!({"timeout_secs": 600})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], id);
}

#[tokio::test]
async fn test_update_network_policy() {
    let app = build_test_app();
    let (_, created) = create_sandbox(app.clone()).await;
    let id = created["id"].as_str().unwrap();

    let (status, body) = json_request(
        app,
        "PATCH",
        &format!("/v1/sandboxes/{id}/network-policy"),
        Some(json!({
            "network_policy": {
                "mode": "deny_all",
                "allow_domains": [],
                "subnets_allow": [],
                "subnets_deny": [],
                "transform_rules": []
            }
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], id);
}

#[tokio::test]
async fn test_exec_command() {
    let app = build_test_app();
    let (_, created) = create_sandbox(app.clone()).await;
    let id = created["id"].as_str().unwrap();

    let (status, body) = json_request(
        app,
        "POST",
        &format!("/v1/sandboxes/{id}/commands"),
        Some(json!({"cmd": "echo", "args": ["hello"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["command_id"].is_string());
}

#[tokio::test]
async fn test_get_command() {
    let app = build_test_app();
    let (_, created) = create_sandbox(app.clone()).await;
    let sandbox_id = created["id"].as_str().unwrap();

    let (_, exec_body) = json_request(
        app.clone(),
        "POST",
        &format!("/v1/sandboxes/{sandbox_id}/commands"),
        Some(json!({"cmd": "echo", "args": ["hello"]})),
    )
    .await;
    let cmd_id = exec_body["command_id"].as_str().unwrap();

    // Small delay to let the mock output task complete
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let (status, body) = json_request(
        app,
        "GET",
        &format!("/v1/sandboxes/{sandbox_id}/commands/{cmd_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["command_id"], cmd_id);
}

#[tokio::test]
async fn test_kill_command() {
    let app = build_test_app();
    let (_, created) = create_sandbox(app.clone()).await;
    let sandbox_id = created["id"].as_str().unwrap();

    let (_, exec_body) = json_request(
        app.clone(),
        "POST",
        &format!("/v1/sandboxes/{sandbox_id}/commands"),
        Some(json!({"cmd": "sleep", "args": ["100"]})),
    )
    .await;
    let cmd_id = exec_body["command_id"].as_str().unwrap();

    let (status, _) = json_request(
        app,
        "POST",
        &format!("/v1/sandboxes/{sandbox_id}/commands/{cmd_id}/kill"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn test_write_and_read_file() {
    let app = build_test_app();
    let (_, created) = create_sandbox(app.clone()).await;
    let id = created["id"].as_str().unwrap();

    // Write file
    let (status, _) = json_request(
        app.clone(),
        "POST",
        &format!("/v1/sandboxes/{id}/files"),
        Some(json!({"path": "/tmp/test.txt", "content": [104, 101, 108, 108, 111]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Read file
    let (status, body) = json_request(
        app,
        "GET",
        &format!("/v1/sandboxes/{id}/files?path=/tmp/test.txt"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["path"], "/tmp/test.txt");
    assert!(body["content"].is_array());
}

#[tokio::test]
async fn test_mkdir() {
    let app = build_test_app();
    let (_, created) = create_sandbox(app.clone()).await;
    let id = created["id"].as_str().unwrap();

    let (status, _) = json_request(
        app,
        "POST",
        &format!("/v1/sandboxes/{id}/dirs"),
        Some(json!({"path": "/tmp/newdir"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn test_create_and_get_snapshot() {
    let app = build_test_app();
    let (_, created) = create_sandbox(app.clone()).await;
    let sandbox_id = created["id"].as_str().unwrap();

    // Create snapshot
    let (status, snap_body) = json_request(
        app.clone(),
        "POST",
        "/v1/snapshots",
        Some(json!({"sandbox_id": sandbox_id, "description": "test snap"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let snap_id = snap_body["id"].as_str().unwrap();
    assert_eq!(snap_body["sandbox_id"], sandbox_id);

    // Get snapshot
    let (status, body) =
        json_request(app, "GET", &format!("/v1/snapshots/{snap_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], snap_id);
}

#[tokio::test]
async fn test_delete_snapshot() {
    let app = build_test_app();
    let (_, created) = create_sandbox(app.clone()).await;
    let sandbox_id = created["id"].as_str().unwrap();

    let (_, snap_body) = json_request(
        app.clone(),
        "POST",
        "/v1/snapshots",
        Some(json!({"sandbox_id": sandbox_id})),
    )
    .await;
    let snap_id = snap_body["id"].as_str().unwrap();

    let (status, _) = json_request(
        app.clone(),
        "DELETE",
        &format!("/v1/snapshots/{snap_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Verify deleted
    let (status, _) = json_request(app, "GET", &format!("/v1/snapshots/{snap_id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_metrics_endpoint() {
    let app = build_test_app();
    let (_, created) = create_sandbox(app.clone()).await;
    let id = created["id"].as_str().unwrap();

    let (status, body) =
        json_request(app, "GET", &format!("/v1/sandboxes/{id}/metrics"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["cpu_usage_percent"], 0.0);
    assert_eq!(body["memory_used_bytes"], 0);
}
