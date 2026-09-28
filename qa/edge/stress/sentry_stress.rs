use inferenced_core::{
    arbiter::Arbiter,
    lease::{LeasePriority, LeaseState},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use serde_json::json;
use std::sync::Arc;

fn make_stress_arbiter() -> Arbiter {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "npu-sentry-stress".into(),
        name: "Sentry Triage NPU".into(),
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
    Arbiter::new(topo)
}

#[tokio::test]
async fn test_edge_sentry_rapid_concurrent_pings() {
    let arbiter = Arc::new(make_stress_arbiter());
    let mut handles = Vec::new();

    // Spawn 15 concurrent emergency triage requests
    for i in 0..15 {
        let arb = arbiter.clone();
        handles.push(tokio::spawn(async move {
            let lease = arb
                .acquire_lease(
                    LeasePriority::EmergencyTriage,
                    128 * 1024 * 1024,
                    None,
                    Some(format!("systemd-sentry-worker-{}", i)),
                    None,
                )
                .await
                .expect("Emergency triage should succeed concurrently");
            assert_eq!(lease.priority, LeasePriority::EmergencyTriage);
            arb.release_lease(lease.id).await.unwrap();
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let topo = arbiter.get_topology().await;
    assert_eq!(
        topo.planes[0].available_memory_bytes,
        2 * 1024 * 1024 * 1024,
        "All emergency memory must be cleanly restored after release"
    );
}

#[tokio::test]
async fn test_edge_sentry_displaces_multiple_batch_leases() {
    let arbiter = make_stress_arbiter();

    // Fill plane with 4 small batch leases
    let mut batch_ids = Vec::new();
    for _ in 0..4 {
        let l = arbiter
            .acquire_lease(
                LeasePriority::Batch,
                512 * 1024 * 1024,
                Some("npu-sentry-stress".into()),
                None,
                None,
            )
            .await
            .unwrap();
        batch_ids.push(l.id);
    }

    let topo_full = arbiter.get_topology().await;
    assert_eq!(topo_full.planes[0].available_memory_bytes, 0);

    // Emergency triage demands 1.5GB -> must preempt 3 batch leases
    let emergency = arbiter
        .acquire_lease(
            LeasePriority::EmergencyTriage,
            1536 * 1024 * 1024,
            None,
            Some("systemd-sentry.service".into()),
            None,
        )
        .await
        .unwrap();

    assert_eq!(emergency.state, LeaseState::Active);

    // Verify at least 3 batch leases were preempted
    let mut preempted_count = 0;
    for id in batch_ids {
        if let Some(lease) = arbiter.get_lease(id).await {
            if lease.state == LeaseState::Preempted {
                preempted_count += 1;
            }
        }
    }
    assert!(preempted_count >= 3, "At least 3 batch leases should be preempted");
}

#[test]
fn test_edge_sentry_payload_serialization_tolerance() {
    // Malformed JSON string fallback handling
    let raw_bytes = b"non-json-kernel-dump-text-crash";
    let payload_json: serde_json::Value = serde_json::from_slice(raw_bytes)
        .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(raw_bytes) }));

    assert_eq!(
        payload_json["raw"].as_str().unwrap(),
        "non-json-kernel-dump-text-crash"
    );
}

#[test]
fn test_edge_sentry_payload_missing_incident_id() {
    let payload = json!({
        "action": "triage_ping"
        // incident_id omitted
    });

    let incident_id = payload
        .get("incident_id")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);

    assert!(incident_id.is_none());
}
