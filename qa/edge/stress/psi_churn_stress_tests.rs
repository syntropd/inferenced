use inferenced_core::{
    arbiter::Arbiter,
    error::Error,
    lease::{LeasePriority, LeaseState},
    psi::{PressureLevel, PressureMetrics, SIMULATED_PSI},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use std::sync::Arc;

fn create_test_topology(total_mem: u64) -> HardwareTopology {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-psi-stress".into(),
        name: "PSI Stress Plane".into(),
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
    topo
}

#[tokio::test]
async fn test_psi_churn_stress_under_critical_pressure() {
    let total_mem = 16 * 1024 * 1024 * 1024; // 16GB
    let arbiter = Arc::new(Arbiter::new(create_test_topology(total_mem)));

    // 1. 50 concurrent tasks attempting non-emergency leases under Critical PSI
    let mut handles = Vec::new();
    for i in 0..50 {
        let arb = arbiter.clone();
        handles.push(tokio::spawn(
            SIMULATED_PSI.scope(PressureLevel::Critical, async move {
                let prio = if i % 2 == 0 {
                    LeasePriority::Interactive
                } else {
                    LeasePriority::Batch
                };
                arb.acquire_lease(
                    prio,
                    128 * 1024 * 1024,
                    Some("plane-psi-stress".into()),
                    Some(format!("client-{}.service", i)),
                    Some(1000 + i as u32),
                )
                .await
            })
        ));
    }

    let mut bus_saturation_count = 0;
    for h in handles {
        let res = h.await.unwrap();
        match res {
            Err(Error::BusSaturation(msg)) => {
                assert!(msg.contains("Memory bus saturated"));
                bus_saturation_count += 1;
            }
            other => panic!("Expected BusSaturation under critical PSI, got {:?}", other),
        }
    }
    assert_eq!(bus_saturation_count, 50, "All 50 non-emergency leases must be throttled");

    // Plane memory must remain completely unallocated
    let topo = arbiter.get_topology().await;
    assert_eq!(topo.planes[0].available_memory_bytes, total_mem);

    // 2. Concurrently submit 10 EmergencyTriage leases - must all succeed
    let mut triage_handles = Vec::new();
    for i in 0..10 {
        let arb = arbiter.clone();
        triage_handles.push(tokio::spawn(
            SIMULATED_PSI.scope(PressureLevel::Critical, async move {
                arb.acquire_lease(
                    LeasePriority::EmergencyTriage,
                    256 * 1024 * 1024,
                    Some("plane-psi-stress".into()),
                    Some(format!("sentry-triage-{}.service", i)),
                    Some(2000 + i as u32),
                )
                .await
            })
        ));
    }

    let mut triage_leases = Vec::new();
    for h in triage_handles {
        let lease = h.await.unwrap().expect("Emergency triage must succeed under critical PSI");
        assert_eq!(lease.priority, LeasePriority::EmergencyTriage);
        triage_leases.push(lease);
    }
    assert_eq!(triage_leases.len(), 10);

    // Release all emergency triage leases
    for l in triage_leases {
        arbiter.release_lease(l.id).await.expect("Release triage lease");
    }

    // Verify zero memory leaks
    let topo_after = arbiter.get_topology().await;
    assert_eq!(
        topo_after.planes[0].available_memory_bytes, total_mem,
        "All memory must be restored after emergency triage leases released"
    );
}

#[tokio::test]
async fn test_psi_transition_from_critical_to_normal_churn() {
    let total_mem = 8 * 1024 * 1024 * 1024; // 8GB
    let arbiter = Arc::new(Arbiter::new(create_test_topology(total_mem)));

    // Under critical PSI: interactive lease fails
    let fail_res = SIMULATED_PSI.scope(PressureLevel::Critical, async {
        arbiter
            .acquire_lease(LeasePriority::Interactive, 128 * 1024 * 1024, None, None, None)
            .await
    }).await;
    assert!(matches!(fail_res, Err(Error::BusSaturation(_))));

    // Transition PSI to Normal: 50 concurrent tasks acquiring and releasing leases
    let mut churn_handles = Vec::new();
    for i in 0..50 {
        let arb = arbiter.clone();
        churn_handles.push(tokio::spawn(
            SIMULATED_PSI.scope(PressureLevel::Normal, async move {
                let lease = arb
                    .acquire_lease(
                        LeasePriority::Interactive,
                        64 * 1024 * 1024,
                        Some("plane-psi-stress".into()),
                        Some(format!("worker-{}.service", i)),
                        Some(3000 + i as u32),
                    )
                    .await
                    .expect("Acquire lease under Normal PSI must succeed");

                // Release lease immediately simulating rapid churn
                arb.release_lease(lease.id).await.expect("Release lease");
                lease.id
            })
        ));
    }

    for h in churn_handles {
        h.await.unwrap();
    }

    // Verify 100% memory recovery and all leases Expired
    let topo = arbiter.get_topology().await;
    assert_eq!(
        topo.planes[0].available_memory_bytes, total_mem,
        "Memory must be 100% recovered after rapid churn"
    );

    let leases = arbiter.list_leases().await;
    assert!(
        leases.iter().all(|l| l.state == LeaseState::Expired),
        "All churn leases must be in Expired state"
    );
}

#[tokio::test]
async fn test_inferenced_simulate_psi_task_local_hook() {
    SIMULATED_PSI
        .scope(PressureLevel::Critical, async {
            let metrics = PressureMetrics::read_current();
            assert_eq!(metrics.level, PressureLevel::Critical);
            assert_eq!(metrics.memory_some_avg10, 60.0);
            assert_eq!(metrics.memory_full_avg10, 25.0);
        })
        .await;

    SIMULATED_PSI
        .scope(PressureLevel::Elevated, async {
            let elevated_metrics = PressureMetrics::read_current();
            assert_eq!(elevated_metrics.level, PressureLevel::Elevated);
            assert_eq!(elevated_metrics.memory_some_avg10, 20.0);
        })
        .await;

    SIMULATED_PSI
        .scope(PressureLevel::Normal, async {
            let normal_metrics = PressureMetrics::read_current();
            assert_eq!(normal_metrics.level, PressureLevel::Normal);
            assert_eq!(normal_metrics.memory_some_avg10, 0.0);
        })
        .await;
}
