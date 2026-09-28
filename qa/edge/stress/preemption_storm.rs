use inferenced_core::{
    arbiter::Arbiter,
    lease::{LeasePriority, LeaseState},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};

#[tokio::test]
async fn test_preemption_storm_under_saturation() {
    let mut topo = HardwareTopology::default();
    let total_mem = 4 * 1024 * 1024 * 1024; // 4GB
    topo.planes.push(ComputePlane {
        id: "gpu-storm".into(),
        name: "Discrete GPU Storm Plane".into(),
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

    // 1. Fully saturate the plane with 4 x 1GB Batch leases
    let mut batch_leases = Vec::new();
    for i in 0..4 {
        let lease = arbiter
            .acquire_lease(
                LeasePriority::Batch,
                1024 * 1024 * 1024,
                Some("gpu-storm".into()),
                Some(format!("batch-{}.service", i)),
                None,
            )
            .await
            .expect("Batch lease acquisition should succeed");
        batch_leases.push(lease);
    }

    let topo_sat = arbiter.get_topology().await;
    assert_eq!(
        topo_sat.planes[0].available_memory_bytes, 0,
        "Plane should be 100% saturated"
    );

    // 2. Submit high priority Interactive leases; should preempt Batch leases
    let mut interactive_leases = Vec::new();
    for i in 0..2 {
        let lease = arbiter
            .acquire_lease(
                LeasePriority::Interactive,
                1024 * 1024 * 1024,
                Some("gpu-storm".into()),
                Some(format!("interactive-{}.service", i)),
                None,
            )
            .await
            .expect("Interactive lease should preempt batch leases and succeed");
        interactive_leases.push(lease);
    }

    // Verify at least 2 batch leases were preempted
    let all_leases = arbiter.list_leases().await;
    let preempted_count = all_leases
        .iter()
        .filter(|l| l.state == LeaseState::Preempted)
        .count();
    assert!(
        preempted_count >= 2,
        "Expected at least 2 preempted batch leases, found {}",
        preempted_count
    );

    // 3. Emergency triage preemption storm
    let triage_lease = arbiter
        .acquire_lease(
            LeasePriority::EmergencyTriage,
            1024 * 1024 * 1024,
            Some("gpu-storm".into()),
            Some("systemd-sentry.service".into()),
            None,
        )
        .await
        .expect("Emergency triage must succeed immediately during storm");

    assert_eq!(triage_lease.priority, LeasePriority::EmergencyTriage);

    // 4. Release triage lease and interactive leases
    arbiter.release_lease(triage_lease.id).await.unwrap();
    for l in interactive_leases {
        arbiter.release_lease(l.id).await.unwrap();
    }
}
