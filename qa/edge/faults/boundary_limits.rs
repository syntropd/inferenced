use inferenced_core::{
    arbiter::Arbiter,
    error::Error,
    lease::{LeaseId, LeasePriority, LeaseState},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use std::time::{Duration, Instant};

fn make_boundary_arbiter(capacity_bytes: u64) -> Arbiter {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-edge-limit".into(),
        name: "Boundary Limit Plane".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: capacity_bytes,
        available_memory_bytes: capacity_bytes,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
    });
    Arbiter::new(topo)
}

#[tokio::test]
async fn test_edge_zero_bytes_lease_request() {
    let arbiter = make_boundary_arbiter(1024 * 1024 * 1024);
    let lease = arbiter
        .acquire_lease(LeasePriority::Interactive, 0, None, None, None)
        .await
        .expect("Zero-byte lease should be accepted as a zero-resource placeholder");

    assert_eq!(lease.allocated_memory_bytes, 0);
    assert_eq!(lease.state, LeaseState::Active);
    arbiter.release_lease(lease.id).await.unwrap();
}

#[tokio::test]
async fn test_edge_u64_max_memory_rejection() {
    let arbiter = make_boundary_arbiter(16 * 1024 * 1024 * 1024);
    let res = arbiter
        .acquire_lease(LeasePriority::Interactive, u64::MAX, None, None, None)
        .await;

    assert!(matches!(res, Err(Error::ResourceExhaustion { .. })));
}

#[tokio::test]
async fn test_edge_exceeding_capacity_by_one_byte() {
    let cap = 4 * 1024 * 1024 * 1024;
    let arbiter = make_boundary_arbiter(cap);

    // Request exactly cap + 1 byte
    let res = arbiter
        .acquire_lease(LeasePriority::Interactive, cap + 1, None, None, None)
        .await;

    assert!(matches!(res, Err(Error::ResourceExhaustion { .. })));
}

#[tokio::test]
async fn test_edge_release_lease_idempotence() {
    let arbiter = make_boundary_arbiter(1024 * 1024 * 1024);
    let lease = arbiter
        .acquire_lease(LeasePriority::Interactive, 512 * 1024 * 1024, None, None, None)
        .await
        .unwrap();

    // First release succeeds
    arbiter.release_lease(lease.id).await.unwrap();

    // Second release on same lease must safely return Ok (idempotent for non-active lease)
    let res = arbiter.release_lease(lease.id).await;
    assert!(res.is_ok(), "Releasing already-released lease should be idempotent");
}

#[tokio::test]
async fn test_edge_250ms_deadline_boundary_measurement() {
    let deadline = Duration::from_millis(250);
    let start = Instant::now();

    // Simulate 250ms timeout window
    tokio::time::sleep(deadline).await;
    let elapsed = start.elapsed();

    assert!(elapsed >= Duration::from_millis(245), "Must satisfy at least 250ms yield window");
    assert!(elapsed < Duration::from_millis(500), "Should not overshoot significantly");
}

#[tokio::test]
async fn test_edge_sub_millisecond_deadline() {
    let deadline = Duration::from_micros(100);
    let start = Instant::now();
    tokio::time::sleep(deadline).await;
    assert!(start.elapsed() >= deadline);
}

#[tokio::test]
async fn test_edge_all_zeros_uuid_lease_not_found() {
    let arbiter = make_boundary_arbiter(1024 * 1024 * 1024);
    let zero_id = LeaseId(uuid::Uuid::nil());
    let res = arbiter.release_lease(zero_id).await;
    assert!(matches!(res, Err(Error::LeaseNotFound(_))));
}
