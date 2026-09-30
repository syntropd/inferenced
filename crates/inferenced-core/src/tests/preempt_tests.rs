use crate::arbiter::Arbiter;
use crate::lease::{LeasePriority, LeaseState};
use crate::preempt::{PreemptCoordinator, PreemptTier};
use crate::topology::{ComputePlane, ComputePlaneKind, HardwareTopology};
use std::sync::Arc;
use std::time::Duration;

fn setup_arbiter() -> Arc<Arbiter> {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-preempt-test".into(),
        name: "Test Preempt Plane".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: 4 * 1024 * 1024 * 1024,
        available_memory_bytes: 4 * 1024 * 1024 * 1024,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
        p2p_links: None,
        kernel_used_memory: 0,
    });
    Arc::new(Arbiter::new(topo))
}

#[tokio::test]
async fn test_preempt_coordinator_cooperative_yield_success() {
    let arbiter = setup_arbiter();
    // Short timeout for fast testing
    let coordinator = PreemptCoordinator::with_deadline(arbiter.clone(), Duration::from_millis(100));

    let lease = arbiter
        .acquire_lease(
            LeasePriority::Batch,
            1024 * 1024 * 1024,
            Some("plane-preempt-test".into()),
            Some("worker.service".into()),
            None,
        )
        .await
        .unwrap();

    let lease_id = lease.id;
    let arb_clone = arbiter.clone();

    // Spawn a simulated client that cooperatively yields after 30ms
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = arb_clone.release_lease(lease_id).await;
    });

    coordinator.preempt_lease(lease_id).await.unwrap();

    let rec = coordinator.get_record(lease_id).await.unwrap();
    assert_eq!(rec.tier, PreemptTier::Tier1Cooperative);
    assert!(rec.completed);
}

#[tokio::test]
async fn test_preempt_coordinator_timeout_fallback_to_freeze() {
    let arbiter = setup_arbiter();
    // 50ms deadline to test timeout escalation
    let coordinator = PreemptCoordinator::with_deadline(arbiter.clone(), Duration::from_millis(50));

    let lease = arbiter
        .acquire_lease(
            LeasePriority::Batch,
            1024 * 1024 * 1024,
            Some("plane-preempt-test".into()),
            Some("stubborn.service".into()),
            None,
        )
        .await
        .unwrap();

    // Client does NOT yield
    coordinator.preempt_lease(lease.id).await.unwrap();

    let rec = coordinator.get_record(lease.id).await.unwrap();
    assert_eq!(rec.tier, PreemptTier::Tier2ForcedFreeze);

    let updated_lease = arbiter.get_lease(lease.id).await.unwrap();
    assert_eq!(updated_lease.state, LeaseState::Frozen);

    // Now test thawing the frozen lease
    coordinator.thaw_lease(lease.id).await.unwrap();
    let thawed = arbiter.get_lease(lease.id).await.unwrap();
    assert_eq!(thawed.state, LeaseState::Active);

    // Now test revoking the lease
    coordinator.revoke_lease(lease.id).await.unwrap();
    let revoked = arbiter.get_lease(lease.id).await.unwrap();
    assert_eq!(revoked.state, LeaseState::Revoked);
}

#[tokio::test]
async fn test_preempt_coordinator_inactive_lease_noop() {
    let arbiter = setup_arbiter();
    let coordinator = PreemptCoordinator::new(arbiter.clone());

    let lease = arbiter
        .acquire_lease(LeasePriority::Batch, 512 * 1024 * 1024, None, None, None)
        .await
        .unwrap();

    // Release immediately
    arbiter.release_lease(lease.id).await.unwrap();

    // Preempting expired lease should succeed as no-op Ok(())
    let res = coordinator.preempt_lease(lease.id).await;
    assert!(res.is_ok());
}
