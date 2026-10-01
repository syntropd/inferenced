use crate::arbiter::{Arbiter, LeaseRequest};
use crate::lease::{LeaseId, LeasePriority, LeaseState};
use crate::topology::{ComputePlane, ComputePlaneKind, HardwareTopology};

fn create_test_topology(plane_id: &str, mem_bytes: u64) -> HardwareTopology {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: plane_id.into(),
        name: "Test GPU Plane".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: mem_bytes,
        available_memory_bytes: mem_bytes,
        numa_node: None,
        supported_formats: vec!["FP16".into()],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec!["test".into()],
        p2p_links: None,
        kernel_used_memory: 0,
    });
    topo
}

#[tokio::test]
async fn test_priority_leasing_hierarchy() {
    assert!(LeasePriority::EmergencyTriage > LeasePriority::Interactive);
    assert!(LeasePriority::Interactive > LeasePriority::Batch);
    assert_eq!(LeasePriority::EmergencyTriage as u32, 100);
    assert_eq!(LeasePriority::Interactive as u32, 10);
    assert_eq!(LeasePriority::Batch as u32, 0);
}

#[tokio::test]
async fn test_clean_borrow_splitting_preemption() {
    let total_mem = 4 * 1024 * 1024 * 1024; // 4GB
    let topo = create_test_topology("gpu-split-test", total_mem);
    let arbiter = Arbiter::new(topo);

    // Acquire batch lease taking 3GB
    let batch_req = LeaseRequest {
        priority: LeasePriority::Batch,
        required_bytes: 3 * 1024 * 1024 * 1024,
        preferred_plane: Some("gpu-split-test".into()),
        client_unit: Some("batch.service".into()),
        client_pid: Some(1234),
    };
    let batch_lease = arbiter.request_lease(batch_req).await.unwrap();
    assert_eq!(batch_lease.state, LeaseState::Active);

    // Verify 1GB remains
    let topo1 = arbiter.get_topology().await;
    assert_eq!(topo1.planes[0].available_memory_bytes, 1024 * 1024 * 1024);

    // Now request 2GB with Interactive priority.
    // Must preempt the 3GB batch lease via clean borrow splitting.
    let interactive_req = LeaseRequest {
        priority: LeasePriority::Interactive,
        required_bytes: 2 * 1024 * 1024 * 1024,
        preferred_plane: Some("gpu-split-test".into()),
        client_unit: Some("interactive.service".into()),
        client_pid: Some(5678),
    };
    let inter_lease = arbiter.request_lease(interactive_req).await.unwrap();
    assert_eq!(inter_lease.state, LeaseState::Active);

    // Verify batch lease was preempted
    let batch_curr = arbiter.get_lease(batch_lease.id).await.unwrap();
    assert_eq!(batch_curr.state, LeaseState::Preempted);

    // Memory: 4GB total - 2GB allocated = 2GB available
    let topo2 = arbiter.get_topology().await;
    assert_eq!(topo2.planes[0].available_memory_bytes, 2 * 1024 * 1024 * 1024);

    // Release interactive lease
    arbiter.release_lease(inter_lease.id).await.unwrap();
    let topo3 = arbiter.get_topology().await;
    assert_eq!(topo3.planes[0].available_memory_bytes, total_mem);
}

#[tokio::test]
async fn test_lease_revocation_returns_resources() {
    let total_mem = 2 * 1024 * 1024 * 1024;
    let topo = create_test_topology("gpu-revoke-test", total_mem);
    let arbiter = Arbiter::new(topo);

    let lease = arbiter
        .acquire_lease(
            LeasePriority::Interactive,
            1024 * 1024 * 1024,
            Some("gpu-revoke-test".into()),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        arbiter.get_topology().await.planes[0].available_memory_bytes,
        1024 * 1024 * 1024
    );

    arbiter.revoke_lease(lease.id).await.unwrap();
    let updated = arbiter.get_lease(lease.id).await.unwrap();
    assert_eq!(updated.state, LeaseState::Revoked);

    assert_eq!(
        arbiter.get_topology().await.planes[0].available_memory_bytes,
        total_mem
    );
}

#[tokio::test]
async fn test_freeze_and_thaw_lease() {
    let topo = create_test_topology("gpu-freeze-test", 1024 * 1024 * 1024);
    let arbiter = Arbiter::new(topo);

    let lease = arbiter
        .acquire_lease(LeasePriority::Interactive, 512 * 1024 * 1024, None, None, None)
        .await
        .unwrap();

    arbiter.freeze_lease(lease.id).await.unwrap();
    let frozen = arbiter.get_lease(lease.id).await.unwrap();
    assert_eq!(frozen.state, LeaseState::Frozen);

    arbiter.thaw_lease(lease.id).await.unwrap();
    let thawed = arbiter.get_lease(lease.id).await.unwrap();
    assert_eq!(thawed.state, LeaseState::Active);
}

#[tokio::test]
async fn test_release_nonexistent_lease_returns_error() {
    let topo = create_test_topology("gpu-err-test", 1024 * 1024 * 1024);
    let arbiter = Arbiter::new(topo);
    let res = arbiter.release_lease(LeaseId::default()).await;
    assert!(res.is_err());
}

#[tokio::test]
async fn test_composite_gang_atomic_allocation() {
    use crate::lease::{CompositeLeaseRequest, GangPolicy, PlaneRole, SliceRequirement};
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "gpu-gang-0".into(),
        name: "GPU 0".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: 4 * 1024 * 1024 * 1024,
        available_memory_bytes: 4 * 1024 * 1024 * 1024,
        numa_node: Some(0),
        supported_formats: vec!["FP16".into()],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
        p2p_links: None,
        kernel_used_memory: 0,
    });
    topo.planes.push(ComputePlane {
        id: "gpu-gang-1".into(),
        name: "GPU 1".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: 4 * 1024 * 1024 * 1024,
        available_memory_bytes: 4 * 1024 * 1024 * 1024,
        numa_node: Some(0),
        supported_formats: vec!["FP16".into()],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
        p2p_links: None,
        kernel_used_memory: 0,
    });

    let arbiter = Arbiter::new(topo);
    let gang_req = CompositeLeaseRequest {
        priority: LeasePriority::Interactive,
        slices: vec![
            SliceRequirement {
                role: PlaneRole::Primary,
                required_bytes: 2 * 1024 * 1024 * 1024,
                preferred_plane: Some("gpu-gang-0".into()),
            },
            SliceRequirement {
                role: PlaneRole::Worker,
                required_bytes: 2 * 1024 * 1024 * 1024,
                preferred_plane: Some("gpu-gang-1".into()),
            },
        ],
        policy: GangPolicy::AllOrNothing,
        client_unit: Some("gang.service".into()),
        client_pid: Some(9999),
    };

    let gang_lease = arbiter.acquire_composite_lease(gang_req).await.unwrap();
    assert_eq!(gang_lease.slices.len(), 2);
    assert_eq!(gang_lease.total_allocated_memory_bytes(), 4 * 1024 * 1024 * 1024);

    let t = arbiter.get_topology().await;
    assert_eq!(t.planes[0].available_memory_bytes, 2 * 1024 * 1024 * 1024);
    assert_eq!(t.planes[1].available_memory_bytes, 2 * 1024 * 1024 * 1024);

    arbiter.release_composite_lease(gang_lease.id).await.unwrap();
    let t2 = arbiter.get_topology().await;
    assert_eq!(t2.planes[0].available_memory_bytes, 4 * 1024 * 1024 * 1024);
    assert_eq!(t2.planes[1].available_memory_bytes, 4 * 1024 * 1024 * 1024);
}

#[tokio::test]
async fn test_resize_lease() {
    let topo = create_test_topology("gpu-resize", 4 * 1024 * 1024 * 1024);
    let arbiter = Arbiter::new(topo);
    let lease = arbiter
        .acquire_lease(
            LeasePriority::Interactive,
            1024 * 1024 * 1024,
            Some("gpu-resize".into()),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(lease.allocated_memory_bytes, 1024 * 1024 * 1024);

    let expanded = arbiter.resize_lease(lease.id, 2 * 1024 * 1024 * 1024).await.unwrap();
    assert_eq!(expanded.allocated_memory_bytes, 2 * 1024 * 1024 * 1024);
    assert_eq!(arbiter.get_topology().await.planes[0].available_memory_bytes, 2 * 1024 * 1024 * 1024);

    let shrunk = arbiter.resize_lease(lease.id, 512 * 1024 * 1024).await.unwrap();
    assert_eq!(shrunk.allocated_memory_bytes, 512 * 1024 * 1024);
    assert_eq!(arbiter.get_topology().await.planes[0].available_memory_bytes, 3584 * 1024 * 1024);
}

