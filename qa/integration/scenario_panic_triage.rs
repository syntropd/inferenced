use inferenced_core::{
    arbiter::Arbiter,
    lease::{LeasePriority, LeaseState},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use serde_json::json;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

#[tokio::test]
async fn test_scenario_emergency_driver_panic_triage() {
    // Scenario 2: GPU driver panics; out-of-band Sentry socket triggers emergency preemption
    let dir = tempdir().unwrap();
    let sentry_socket_path = dir.path().join("sentry_panic.sock");
    let listener = UnixListener::bind(&sentry_socket_path).unwrap();

    let mut topo = HardwareTopology::default();
    // 1. Unstable discrete GPU (simulated driver crash)
    topo.planes.push(ComputePlane {
        id: "gpu-panicked".into(),
        name: "Discrete GPU (Kernel Panic)".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: 8 * 1024 * 1024 * 1024,
        available_memory_bytes: 0, // fully locked up
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
        p2p_links: None,
        kernel_used_memory: 0,
    });

    // 2. Dedicated out-of-band NPU enclave reserved for systemd-sentry
    topo.planes.push(ComputePlane {
        id: "npu-sentry-enclave".into(),
        name: "Dedicated Sentry NPU Triage Enclave".into(),
        kind: ComputePlaneKind::NpuAccelerator,
        device_path: None,
        total_memory_bytes: 2 * 1024 * 1024 * 1024,
        available_memory_bytes: 2 * 1024 * 1024 * 1024,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: true,
        is_quarantined: false,
        hardware_features: vec![],
        p2p_links: None,
        kernel_used_memory: 0,
    });

    let arbiter = Arc::new(Arbiter::new(topo));

    // Fill NPU plane with speculative batch lease to test preemption
    let batch_lease = arbiter
        .acquire_lease(
            LeasePriority::Batch,
            2 * 1024 * 1024 * 1024,
            Some("npu-sentry-enclave".into()),
            Some("background-worker.service".into()),
            None,
        )
        .await
        .expect("Batch lease on NPU");

    assert_eq!(batch_lease.state, LeaseState::Active);

    // Spawn Sentry triage server on out-of-band socket
    let server_arbiter = arbiter.clone();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.unwrap();
        let incident: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();

        assert_eq!(incident["incident_id"], "drm-amdgpu-page-fault-ring-gfx");

        // Out-of-band emergency preemption
        let triage_lease = server_arbiter
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
            "incident_id": incident["incident_id"],
            "allocated_plane": triage_lease.plane_id,
            "analysis": "Emergency compute isolated on NPU enclave. GPU hang isolated."
        });

        stream.write_all(&serde_json::to_vec(&resp).unwrap()).await.unwrap();
        server_arbiter.release_lease(triage_lease.id).await.unwrap();
    });

    // Client (systemd-sentry) detects crash, establishes emergency connection
    let mut client = UnixStream::connect(&sentry_socket_path).await.unwrap();
    let emergency_req = json!({
        "action": "emergency_triage",
        "incident_id": "drm-amdgpu-page-fault-ring-gfx",
        "unit_name": "systemd-sentry.service",
        "dmesg_snippet": "amdgpu 0000:03:00.0: GPU reset begin!"
    });

    client.write_all(&serde_json::to_vec(&emergency_req).unwrap()).await.unwrap();

    let mut resp_buf = vec![0u8; 4096];
    let n = client.read(&mut resp_buf).await.unwrap();
    let triage_resp: serde_json::Value = serde_json::from_slice(&resp_buf[..n]).unwrap();

    assert_eq!(triage_resp["status"], "accepted");
    assert_eq!(triage_resp["allocated_plane"], "npu-sentry-enclave");
    assert!(triage_resp["analysis"].as_str().unwrap().contains("GPU hang isolated"));

    // Verify batch lease was preempted to guarantee enclave isolation
    let batch_status = arbiter.get_lease(batch_lease.id).await.unwrap();
    assert_eq!(batch_status.state, LeaseState::Preempted);
}
