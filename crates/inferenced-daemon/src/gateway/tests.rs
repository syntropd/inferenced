use super::*;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use inferenced_core::{
    arbiter::Arbiter,
    preempt::PreemptCoordinator,
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use tower::ServiceExt;

fn make_test_state() -> Arc<AppState> {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-gateway-test".into(),
        name: "Gateway GPU".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: 8 * 1024 * 1024 * 1024,
        available_memory_bytes: 8 * 1024 * 1024 * 1024,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
    });
    let arbiter = Arc::new(Arbiter::new(topo));
    let preempt = Arc::new(PreemptCoordinator::new(arbiter.clone()));
    Arc::new(AppState { arbiter, preempt, api_token: None })
}

#[tokio::test]
async fn test_gateway_health_endpoint() {
    let state = make_test_state();
    let app = build_gateway_router(state);

    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["planes"], 1);
}

#[tokio::test]
async fn test_gateway_serve_unix_socket_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let sock_path = dir.path().join("gateway_test.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();
    let state = make_test_state();
    let app = build_gateway_router(state);

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let server_task = tokio::spawn(async move {
        serve_gateway(
            GatewayListener::Unix(listener),
            app,
            async move { let _ = shutdown_rx.await; },
        ).await
    });

    let mut client = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
    tokio::io::AsyncWriteExt::write_all(
        &mut client,
        b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    ).await.unwrap();

    let mut response_bytes = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut client, &mut response_bytes).await.unwrap();
    let response_str = String::from_utf8_lossy(&response_bytes);
    assert!(response_str.starts_with("HTTP/1.1 200 OK"));
    assert!(response_str.contains("\"status\":\"ok\""));

    let _ = shutdown_tx.send(());
    let _ = server_task.await;
}
