use inferenced_core::{
    arbiter::Arbiter,
    error::Error,
    lease::{ComputeLease, LeasePriority, LeaseState},
    psi::{PressureLevel, SIMULATED_PSI},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Barrier;

fn create_psi_topology(total_mem: u64) -> HardwareTopology {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-psi-adversarial".into(),
        name: "Adversarial PSI Test Plane".into(),
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

fn create_uma_cpu_topology(capacity_bytes: u64) -> HardwareTopology {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-uma-adv".into(),
        name: "Adversarial UMA Plane".into(),
        kind: ComputePlaneKind::IntegratedUma,
        device_path: None,
        total_memory_bytes: capacity_bytes,
        available_memory_bytes: capacity_bytes,
        numa_node: Some(0),
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec!["unified_memory".into()],
    });
    topo.planes.push(ComputePlane {
        id: "plane-cpu-adv".into(),
        name: "Adversarial CPU Plane".into(),
        kind: ComputePlaneKind::CpuMatrixExtension,
        device_path: None,
        total_memory_bytes: capacity_bytes,
        available_memory_bytes: capacity_bytes,
        numa_node: Some(0),
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec!["avx512".into()],
    });
    topo
}

async fn check_dual_accounting(arbiter: &Arbiter) {
    let topo = arbiter.get_topology().await;
    let uma = topo.planes.iter().find(|p| p.kind == ComputePlaneKind::IntegratedUma).unwrap();
    let cpu = topo.planes.iter().find(|p| p.kind == ComputePlaneKind::CpuMatrixExtension).unwrap();
    assert_eq!(
        uma.available_memory_bytes, cpu.available_memory_bytes,
        "Invariant violated: UMA {} != CPU {}",
        uma.available_memory_bytes, cpu.available_memory_bytes
    );
}

#[tokio::test]
async fn test_adversarial_rapid_psi_oscillation_under_extreme_churn() {
    let total_mem = 8 * 1024 * 1024 * 1024; // 8GB
    let arbiter = Arc::new(Arbiter::new(create_psi_topology(total_mem)));
    let stop_flag = Arc::new(AtomicBool::new(false));

    // 1. Critical PSI rejects non-emergency requests with BusSaturation
    let mut rejected = 0;
    for _ in 0..20 {
        let res = SIMULATED_PSI.scope(PressureLevel::Critical, async {
            arbiter.acquire_lease(LeasePriority::Interactive, 128 * 1024 * 1024, None, None, None).await
        }).await;
        if matches!(res, Err(Error::BusSaturation(_))) {
            rejected += 1;
        }
    }
    assert_eq!(rejected, 20, "All 20 non-emergency requests under Critical PSI must be rejected");

    // 2. EmergencyTriage succeeds even under Critical PSI
    let mut triage_leases = Vec::new();
    for i in 0..10 {
        let arb = arbiter.clone();
        let lease = SIMULATED_PSI.scope(PressureLevel::Critical, async move {
            arb.acquire_lease(
                LeasePriority::EmergencyTriage,
                128 * 1024 * 1024,
                Some("plane-psi-adversarial".into()),
                Some(format!("sentry-triage-{}.service", i)),
                Some(9000 + i as u32),
            ).await
        }).await.expect("EmergencyTriage must succeed under Critical PSI");
        triage_leases.push(lease);
    }
    assert_eq!(triage_leases.len(), 10);

    // Release all emergency triage leases
    for l in triage_leases {
        arbiter.release_lease(l.id).await.expect("Release triage lease");
    }

    // 3. Concurrent rapid churn under Normal PSI
    let mut workers = Vec::new();
    for i in 0..60 {
        let arb = arbiter.clone();
        workers.push(tokio::spawn(
            SIMULATED_PSI.scope(PressureLevel::Normal, async move {
                let lease = arb.acquire_lease(
                    LeasePriority::Interactive,
                    64 * 1024 * 1024,
                    Some("plane-psi-adversarial".into()),
                    Some(format!("churn-client-{}.service", i)),
                    Some(8000 + i as u32),
                ).await.expect("Acquire lease under Normal PSI");
                arb.release_lease(lease.id).await.expect("Release lease");
            })
        ));
    }

    for w in workers {
        w.await.unwrap();
    }

    stop_flag.store(true, Ordering::SeqCst);

    // Invariant: 100% memory restored, zero zombie leases
    let topo = arbiter.get_topology().await;
    assert_eq!(topo.planes[0].available_memory_bytes, total_mem, "Memory leak detected!");
    let leases = arbiter.list_leases().await;
    assert!(leases.iter().all(|l| l.state == LeaseState::Expired), "Zombie leases detected!");
}

#[tokio::test]
async fn test_adversarial_uma_cpu_preemption_and_mass_thaw_storm() {
    let capacity = 16 * 1024 * 1024 * 1024; // 16GB
    let arbiter = Arc::new(Arbiter::new(create_uma_cpu_topology(capacity)));
    check_dual_accounting(&arbiter).await;

    // Phase 1: 16 concurrent tasks allocate 1GB Batch leases on UMA plane (100% saturation)
    let barrier = Arc::new(Barrier::new(16));
    let mut batch_handles = Vec::new();
    for i in 0..16 {
        let arb = arbiter.clone();
        let b = barrier.clone();
        batch_handles.push(tokio::spawn(async move {
            b.wait().await;
            arb.acquire_lease(
                LeasePriority::Batch,
                1024 * 1024 * 1024,
                Some("plane-uma-adv".into()),
                Some(format!("batch-task-{}.service", i)),
                Some(400 + i as u32),
            ).await
        }));
    }

    let mut batch_leases: Vec<ComputeLease> = Vec::new();
    for h in batch_handles {
        batch_leases.push(h.await.unwrap().expect("Batch lease acquire"));
    }
    assert_eq!(batch_leases.len(), 16);

    // Invariant: both UMA and CPU available memory is exactly 0
    check_dual_accounting(&arbiter).await;
    let topo_sat = arbiter.get_topology().await;
    assert_eq!(topo_sat.planes[0].available_memory_bytes, 0);
    assert_eq!(topo_sat.planes[1].available_memory_bytes, 0);

    // Phase 2: 8 concurrent tasks allocate 2GB Interactive leases, preempting all 16 batch leases
    let barrier_preempt = Arc::new(Barrier::new(8));
    let mut interactive_handles = Vec::new();
    for i in 0..8 {
        let arb = arbiter.clone();
        let b = barrier_preempt.clone();
        interactive_handles.push(tokio::spawn(async move {
            b.wait().await;
            arb.acquire_lease(
                LeasePriority::Interactive,
                2 * 1024 * 1024 * 1024,
                Some("plane-uma-adv".into()),
                Some(format!("interactive-task-{}.service", i)),
                Some(600 + i as u32),
            ).await
        }));
    }

    let mut interactive_leases: Vec<ComputeLease> = Vec::new();
    for h in interactive_handles {
        interactive_leases.push(h.await.unwrap().expect("Interactive lease acquire"));
    }
    assert_eq!(interactive_leases.len(), 8);

    // All 16 batch leases must now be in Preempted state
    let all_leases = arbiter.list_leases().await;
    let preempted_count = all_leases.iter().filter(|l| l.state == LeaseState::Preempted).count();
    assert_eq!(preempted_count, 16, "All 16 batch leases must be Preempted");
    check_dual_accounting(&arbiter).await;

    // Phase 3: Mass thaw storm in parallel with staggered interactive releases
    let release_handles: Vec<_> = interactive_leases.into_iter().enumerate().map(|(idx, l)| {
        let arb = arbiter.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(3 * (idx + 1) as u64)).await;
            arb.release_lease(l.id).await.expect("Interactive release");
        })
    }).collect();

    let thaw_handles: Vec<_> = batch_leases.iter().map(|bl| {
        let arb = arbiter.clone();
        let lid = bl.id;
        tokio::spawn(async move {
            loop {
                match arb.thaw_lease(lid).await {
                    Ok(()) => return,
                    Err(Error::ResourceExhaustion { .. }) => {
                        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                    }
                    Err(e) => panic!("Unexpected error during thaw: {:?}", e),
                }
            }
        })
    }).collect();

    for h in release_handles { h.await.unwrap(); }
    for h in thaw_handles { h.await.unwrap(); }

    // After all thaws, dual accounting invariant must strictly hold
    check_dual_accounting(&arbiter).await;
    let topo_thawed = arbiter.get_topology().await;
    assert_eq!(topo_thawed.planes[0].available_memory_bytes, 0);
    assert_eq!(topo_thawed.planes[1].available_memory_bytes, 0);

    // Phase 4: Final release of all batch leases
    for bl in batch_leases {
        arbiter.release_lease(bl.id).await.expect("Batch release");
    }

    check_dual_accounting(&arbiter).await;
    let topo_final = arbiter.get_topology().await;
    assert_eq!(topo_final.planes[0].available_memory_bytes, capacity);
    assert_eq!(topo_final.planes[1].available_memory_bytes, capacity);
}
