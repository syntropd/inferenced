use inferenced_core::{
    arbiter::Arbiter,
    lease::{LeasePriority, LeaseState},
    model::{ModelDescriptor, ModelPlacementState, ModelRegistry},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tempfile::tempdir;

#[tokio::test]
async fn test_sentry_emergency_lease_bypasses_exhaustion() {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "cpu-matrix-triage".into(),
        name: "Dedicated Host CPU".into(),
        kind: ComputePlaneKind::CpuMatrixExtension,
        device_path: None,
        total_memory_bytes: 2 * 1024 * 1024 * 1024,
        available_memory_bytes: 0, // completely exhausted
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: true,
        is_quarantined: false,
        hardware_features: vec!["AMX-Tile".into()],
    });

    let arbiter = Arbiter::new(topo);
    let lease = arbiter
        .acquire_lease(
            LeasePriority::EmergencyTriage,
            1024 * 1024 * 1024,
            None,
            Some("systemd-sentry.service".into()),
            None,
        )
        .await
        .expect("Emergency triage must succeed despite exhaustion");

    assert_eq!(lease.priority, LeasePriority::EmergencyTriage);
    assert_eq!(lease.plane_id, "cpu-matrix-triage");
    assert_eq!(lease.state, LeaseState::Active);
}

#[tokio::test]
async fn test_sentry_model_pinning() {
    let mut reg = ModelRegistry::new();
    let desc = ModelDescriptor {
        id: "sentry-triage-deepseek:1.5b".into(),
        format: "GGUF".into(),
        path: PathBuf::from("/var/lib/systemd-sentry/triage.gguf"),
        estimated_memory_bytes: 1536 * 1024 * 1024,
        placement: ModelPlacementState::Dormant,
        resident_plane_id: None,
        last_accessed: chrono::Utc::now(),
        access_count: 0,
        preferred_plane: None,
    };

    reg.register(desc);
    let pinned = reg.pin_for_triage("sentry-triage-deepseek:1.5b", "npu-pinned-slice".into());
    assert!(pinned);

    let fetched = reg.get("sentry-triage-deepseek:1.5b").unwrap();
    assert_eq!(fetched.placement, ModelPlacementState::PinnedTriage);
    assert_eq!(fetched.resident_plane_id, Some("npu-pinned-slice".into()));
}

#[tokio::test]
async fn test_sentry_triage_socket_protocol_roundtrip() {
    let dir = tempdir().unwrap();
    let socket_path = dir.path().join("sentry_test.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "npu-triage-0".into(),
        name: "Dedicated Sentry NPU".into(),
        kind: ComputePlaneKind::NpuAccelerator,
        device_path: None,
        total_memory_bytes: 4 * 1024 * 1024 * 1024,
        available_memory_bytes: 4 * 1024 * 1024 * 1024,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: true,
        is_quarantined: false,
        hardware_features: vec![],
    });

    let arbiter = Arc::new(Arbiter::new(topo));
    let server_arbiter = arbiter.clone();

    // Spawn mock triage server
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 1024];
        let n = stream.read(&mut buf).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(payload["action"], "triage_ping");

        let lease = server_arbiter
            .acquire_lease(
                LeasePriority::EmergencyTriage,
                1024 * 1024 * 1024,
                None,
                Some("systemd-sentry.service".into()),
                None,
            )
            .await
            .unwrap();

        let resp = json!({
            "status": "accepted",
            "allocated_plane": lease.plane_id,
            "incident_id": payload.get("incident_id"),
        });

        stream.write_all(&serde_json::to_vec(&resp).unwrap()).await.unwrap();
        server_arbiter.release_lease(lease.id).await.unwrap();
    });

    // Client connects and sends triage ping
    let mut client = UnixStream::connect(&socket_path).await.unwrap();
    let req = json!({
        "action": "triage_ping",
        "incident_id": "panic-drm-amdgpu-001"
    });
    client.write_all(&serde_json::to_vec(&req).unwrap()).await.unwrap();

    let mut resp_buf = vec![0u8; 1024];
    let n = client.read(&mut resp_buf).await.unwrap();
    let resp: serde_json::Value = serde_json::from_slice(&resp_buf[..n]).unwrap();

    assert_eq!(resp["status"], "accepted");
    assert_eq!(resp["allocated_plane"], "npu-triage-0");
    assert_eq!(resp["incident_id"], "panic-drm-amdgpu-001");
}

#[tokio::test]
async fn test_sentry_non_reserved_planes_unaffected_by_sentry_reservation() {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "gpu-workload".into(),
        name: "General Workload dGPU".into(),
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
    topo.planes.push(ComputePlane {
        id: "npu-triage".into(),
        name: "Sentry Triage Enclave".into(),
        kind: ComputePlaneKind::NpuAccelerator,
        device_path: None,
        total_memory_bytes: 2 * 1024 * 1024 * 1024,
        available_memory_bytes: 2 * 1024 * 1024 * 1024,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: true,
        is_quarantined: false,
        hardware_features: vec![],
    });

    let arbiter = Arbiter::new(topo);
    // General workload lease must NOT take the triage-reserved plane
    let general_lease = arbiter
        .acquire_lease(LeasePriority::Interactive, 4 * 1024 * 1024 * 1024, None, None, None)
        .await
        .unwrap();

    assert_eq!(general_lease.plane_id, "gpu-workload");

    // Sentry emergency lease chooses the triage-reserved plane
    let sentry_lease = arbiter
        .acquire_lease(LeasePriority::EmergencyTriage, 1024 * 1024 * 1024, None, None, None)
        .await
        .unwrap();

    assert_eq!(sentry_lease.plane_id, "npu-triage");
}
