use inferenced_core::{
    arbiter::Arbiter,
    error::Error,
    lease::{ComputeLease, LeasePriority, LeaseState},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use std::sync::Arc;
use tokio::sync::Barrier;

fn create_uma_cpu_topology(capacity_bytes: u64) -> HardwareTopology {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-uma-0".into(),
        name: "Integrated UMA Plane".into(),
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
        id: "plane-cpu-0".into(),
        name: "CPU Matrix Extension".into(),
        kind: ComputePlaneKind::CpuMatrixExtension,
        device_path: None,
        total_memory_bytes: capacity_bytes,
        available_memory_bytes: capacity_bytes,
        numa_node: Some(0),
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec!["amx".into()],
    });
    topo
}

async fn assert_dual_accounting_invariant(arbiter: &Arbiter) {
    let topo = arbiter.get_topology().await;
    let uma = topo
        .planes
        .iter()
        .find(|p| p.kind == ComputePlaneKind::IntegratedUma)
        .expect("IntegratedUma plane present");
    let cpu = topo
        .planes
        .iter()
        .find(|p| p.kind == ComputePlaneKind::CpuMatrixExtension)
        .expect("CpuMatrixExtension plane present");

    assert_eq!(
        uma.available_memory_bytes, cpu.available_memory_bytes,
        "Invariant violated: UMA available ({}) != CPU available ({})",
        uma.available_memory_bytes, cpu.available_memory_bytes
    );
}

#[tokio::test]
async fn test_uma_preemption_and_concurrent_thaw_storm() {
    let plane_size = 8 * 1024 * 1024 * 1024; // 8 GB
    let arbiter = Arc::new(Arbiter::new(create_uma_cpu_topology(plane_size)));

    assert_dual_accounting_invariant(&arbiter).await;

    // Phase 1: 8 concurrent tasks acquire 1GB Batch leases, fully saturating the 8GB plane
    let barrier_batch = Arc::new(Barrier::new(8));
    let mut batch_handles = Vec::new();
    for i in 0..8 {
        let arb = arbiter.clone();
        let b = barrier_batch.clone();
        batch_handles.push(tokio::spawn(async move {
            b.wait().await;
            arb.acquire_lease(
                LeasePriority::Batch,
                1024 * 1024 * 1024,
                Some("plane-uma-0".into()),
                Some(format!("batch-worker-{}.service", i)),
                Some(100 + i as u32),
            )
            .await
        }));
    }

    let mut batch_leases: Vec<ComputeLease> = Vec::new();
    for h in batch_handles {
        let lease = h.await.unwrap().expect("Batch lease acquisition failed");
        batch_leases.push(lease);
    }
    assert_eq!(batch_leases.len(), 8);

    // Verify plane is 100% saturated on both UMA and CPU
    assert_dual_accounting_invariant(&arbiter).await;
    let topo = arbiter.get_topology().await;
    assert_eq!(topo.planes[0].available_memory_bytes, 0);
    assert_eq!(topo.planes[1].available_memory_bytes, 0);

    // Phase 2: 4 concurrent tasks submit 2GB Interactive leases, preempting all 8 Batch leases
    let barrier_preempt = Arc::new(Barrier::new(4));
    let mut interactive_handles = Vec::new();
    for i in 0..4 {
        let arb = arbiter.clone();
        let b = barrier_preempt.clone();
        interactive_handles.push(tokio::spawn(async move {
            b.wait().await;
            arb.acquire_lease(
                LeasePriority::Interactive,
                2 * 1024 * 1024 * 1024,
                Some("plane-uma-0".into()),
                Some(format!("interactive-worker-{}.service", i)),
                Some(200 + i as u32),
            )
            .await
        }));
    }

    let mut interactive_leases = Vec::new();
    for h in interactive_handles {
        let lease = h.await.unwrap().expect("Interactive lease acquisition failed");
        interactive_leases.push(lease);
    }
    assert_eq!(interactive_leases.len(), 4);

    // Invariant check: both planes must still agree
    assert_dual_accounting_invariant(&arbiter).await;

    // Verify all 8 batch leases are now Preempted
    let all_leases = arbiter.list_leases().await;
    let preempted_count = all_leases
        .iter()
        .filter(|l| l.state == LeaseState::Preempted)
        .count();
    assert_eq!(preempted_count, 8, "All 8 batch leases must be Preempted");

    // Phase 3: Simultaneous Thawing Storm with racing releases
    let release_handles: Vec<_> = interactive_leases
        .into_iter()
        .enumerate()
        .map(|(idx, lease)| {
            let arb = arbiter.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(5 * (idx + 1) as u64)).await;
                arb.release_lease(lease.id).await.expect("Interactive release");
            })
        })
        .collect();

    let thaw_handles: Vec<_> = batch_leases
        .iter()
        .map(|batch_lease| {
            let arb = arbiter.clone();
            let lid = batch_lease.id;
            tokio::spawn(async move {
                let mut attempts = 0;
                loop {
                    match arb.thaw_lease(lid).await {
                        Ok(()) => return attempts,
                        Err(Error::ResourceExhaustion { .. }) => {
                            attempts += 1;
                            tokio::time::sleep(std::time::Duration::from_millis(3)).await;
                        }
                        Err(e) => panic!("Unexpected error during thaw: {:?}", e),
                    }
                }
            })
        })
        .collect();

    for h in release_handles {
        h.await.unwrap();
    }

    for h in thaw_handles {
        let _ = h.await.unwrap();
    }

    // Dual accounting invariant MUST hold during and after the thaw storm
    assert_dual_accounting_invariant(&arbiter).await;

    // All 8 batch leases are active again, plane is fully occupied (0 bytes free on both)
    let topo_thawed = arbiter.get_topology().await;
    assert_eq!(topo_thawed.planes[0].available_memory_bytes, 0);
    assert_eq!(topo_thawed.planes[1].available_memory_bytes, 0);

    // Phase 4: Final cleanup: release all 8 batch leases
    for l in batch_leases {
        arbiter.release_lease(l.id).await.expect("Release batch lease");
    }

    // Invariant check: 100% memory recovery
    assert_dual_accounting_invariant(&arbiter).await;
    let topo_final = arbiter.get_topology().await;
    assert_eq!(topo_final.planes[0].available_memory_bytes, plane_size);
    assert_eq!(topo_final.planes[1].available_memory_bytes, plane_size);
}
