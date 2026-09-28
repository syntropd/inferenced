use inferenced_core::{
    arbiter::Arbiter,
    error::Error,
    lease::{LeaseId, LeasePriority, LeaseState},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};

#[tokio::test]
async fn test_dead_client_lease_reclaim_lifecycle() {
    let mut topo = HardwareTopology::default();
    let total_mem = 8 * 1024 * 1024 * 1024; // 8GB
    topo.planes.push(ComputePlane {
        id: "gpu-reclaim".into(),
        name: "Discrete GPU Reclaim Plane".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: total_mem,
        available_memory_bytes: total_mem,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
    });

    let arbiter = Arbiter::new(topo);

    // Acquire lease for client process PID 99999
    let lease = arbiter
        .acquire_lease(
            LeasePriority::Interactive,
            2 * 1024 * 1024 * 1024, // 2GB
            Some("gpu-reclaim".into()),
            Some("ephemeral-worker.service".into()),
            Some(99999),
        )
        .await
        .expect("Acquire should succeed");

    assert_eq!(
        arbiter.get_topology().await.planes[0].available_memory_bytes,
        6 * 1024 * 1024 * 1024
    );

    // Client process dies / socket disconnects: Reclaim lease
    arbiter
        .release_lease(lease.id)
        .await
        .expect("Release dead client lease should succeed");

    // Check memory fully restored
    let current_topo = arbiter.get_topology().await;
    assert_eq!(
        current_topo.planes[0].available_memory_bytes, total_mem,
        "All memory must be restored to plane"
    );

    let leases = arbiter.list_leases().await;
    let released = leases.iter().find(|l| l.id == lease.id).unwrap();
    assert_eq!(released.state, LeaseState::Expired);

    // Idempotent double release should not double-credit memory
    arbiter
        .release_lease(lease.id)
        .await
        .expect("Re-releasing expired lease should be idempotent Ok");
    assert_eq!(
        arbiter.get_topology().await.planes[0].available_memory_bytes,
        total_mem
    );

    // Releasing completely unknown lease returns LeaseNotFound
    let unknown_id = LeaseId::default();
    let err = arbiter.release_lease(unknown_id).await;
    assert!(matches!(err, Err(Error::LeaseNotFound(_))));
}
