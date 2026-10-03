use inferenced_core::{
    arbiter::Arbiter,
    freezer::{send_cooperative_yield_signal, signal_process},
    lease::{LeasePriority, LeaseState},
    topology::{ComputePlane, HardwareTopology},
};
use rustix::process::Signal;

fn make_test_arbiter(capacity_bytes: u64) -> Arbiter {
    let mut topo = HardwareTopology::default();
    topo.planes.push(
        ComputePlane::builder("plane-test-gpu")
            .name("Test Discrete GPU")
            .no_device_path()
            .total_memory(capacity_bytes)
            .numa_node(None)
            .supported_formats(vec![])
            .hardware_features(vec![])
            .build(),
    );
    Arbiter::new(topo)
}

#[tokio::test]
async fn test_preempt_cooperative_yield_state_transition() {
    inferenced_core::psi::SIMULATED_PSI.scope(inferenced_core::psi::PressureLevel::Normal, async {
        let arbiter = make_test_arbiter(8 * 1024 * 1024 * 1024);
        let lease = arbiter
            .acquire_lease(LeasePriority::Batch, 2 * 1024 * 1024 * 1024, None, None, None)
            .await
            .expect("Acquire should succeed");

        assert_eq!(lease.state, LeaseState::Active);
        arbiter.yield_lease(lease.id).await.expect("Yield should succeed");

        let updated = arbiter.get_lease(lease.id).await.expect("Lease exists");
        assert_eq!(updated.state, LeaseState::Preempting);
    }).await;
}

#[tokio::test]
async fn test_preempt_freeze_and_thaw_lease_state() {
    let arbiter = make_test_arbiter(8 * 1024 * 1024 * 1024);
    let lease = arbiter
        .acquire_lease(LeasePriority::Batch, 1024 * 1024 * 1024, None, None, None)
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
async fn test_preempt_priority_preemption_reclaims_memory() {
    let total_bytes = 4 * 1024 * 1024 * 1024;
    let arbiter = make_test_arbiter(total_bytes);

    // 1. Fill memory with Batch priority lease
    let batch_lease = arbiter
        .acquire_lease(LeasePriority::Batch, total_bytes, None, Some("batch.service".into()), None)
        .await
        .expect("Batch lease acquire");

    let topo_before = arbiter.get_topology().await;
    assert_eq!(topo_before.planes[0].available_memory_bytes, 0);

    // 2. High priority Interactive lease arrives, forcing preemption of batch lease
    let high_prio_lease = arbiter
        .acquire_lease(LeasePriority::Interactive, total_bytes, None, Some("user.service".into()), None)
        .await
        .expect("Interactive lease should preempt batch lease");

    assert_eq!(high_prio_lease.priority, LeasePriority::Interactive);

    let batch_after = arbiter.get_lease(batch_lease.id).await.expect("Batch lease status");
    assert_eq!(batch_after.state, LeaseState::Preempted);
}

#[tokio::test]
async fn test_preempt_cooperative_yield_signal_dispatch() {
    // Attempting to signal a non-existent PID (e.g. 999999) safely returns error
    let res = send_cooperative_yield_signal(999999);
    assert!(res.is_err(), "Non-existent PID signal should return error");

    // Attempting with PID 0 / invalid
    let res_zero = signal_process(0, Signal::Usr1);
    // rustix Pid::from_raw(0) returns None
    assert!(res_zero.is_err());
}

#[tokio::test]
async fn test_preempt_multiple_lower_priority_leases_preempted_in_order() {
    let total_bytes = 6 * 1024 * 1024 * 1024;
    let arbiter = make_test_arbiter(total_bytes);

    // Acquire two batch leases of 2GB each
    let b1 = arbiter
        .acquire_lease(LeasePriority::Batch, 2 * 1024 * 1024 * 1024, None, None, None)
        .await
        .unwrap();
    let b2 = arbiter
        .acquire_lease(LeasePriority::Batch, 2 * 1024 * 1024 * 1024, None, None, None)
        .await
        .unwrap();

    // Now request 5GB with Interactive priority - both batch leases must be preempted
    let high = arbiter
        .acquire_lease(LeasePriority::Interactive, 5 * 1024 * 1024 * 1024, None, None, None)
        .await
        .unwrap();

    assert_eq!(high.state, LeaseState::Active);
    assert_eq!(arbiter.get_lease(b1.id).await.unwrap().state, LeaseState::Preempted);
    assert_eq!(arbiter.get_lease(b2.id).await.unwrap().state, LeaseState::Preempted);
}

#[tokio::test]
async fn test_preempt_thaw_preempted_lease_memory_safety() {
    let total_bytes = 4 * 1024 * 1024 * 1024;
    let arbiter = make_test_arbiter(total_bytes);

    let batch = arbiter
        .acquire_lease(LeasePriority::Batch, total_bytes, None, None, None)
        .await
        .unwrap();

    let interactive = arbiter
        .acquire_lease(LeasePriority::Interactive, total_bytes, None, None, None)
        .await
        .unwrap();

    assert_eq!(arbiter.get_lease(batch.id).await.unwrap().state, LeaseState::Preempted);

    // Attempting to thaw batch lease while memory is fully occupied must fail
    let thaw_err = arbiter.thaw_lease(batch.id).await;
    assert!(thaw_err.is_err(), "Thawing preempted lease with no available memory must fail");

    // Release interactive lease, freeing 4GB
    arbiter.release_lease(interactive.id).await.unwrap();

    // Now thawing batch lease succeeds and deducts memory
    arbiter.thaw_lease(batch.id).await.unwrap();
    let thawed = arbiter.get_lease(batch.id).await.unwrap();
    assert_eq!(thawed.state, LeaseState::Active);

    let topo = arbiter.get_topology().await;
    assert_eq!(topo.planes[0].available_memory_bytes, 0);

    // Releasing thawed lease restores memory cleanly without double reclaim
    arbiter.release_lease(batch.id).await.unwrap();
    let topo_final = arbiter.get_topology().await;
    assert_eq!(topo_final.planes[0].available_memory_bytes, total_bytes);
}

#[tokio::test]
async fn test_preempt_revoke_preempted_lease_no_double_reclaim() {
    let total_bytes = 4 * 1024 * 1024 * 1024;
    let arbiter = make_test_arbiter(total_bytes);

    let batch = arbiter
        .acquire_lease(LeasePriority::Batch, total_bytes, None, None, None)
        .await
        .unwrap();

    let interactive = arbiter
        .acquire_lease(LeasePriority::Interactive, total_bytes, None, None, None)
        .await
        .unwrap();

    assert_eq!(arbiter.get_lease(batch.id).await.unwrap().state, LeaseState::Preempted);
    let topo_mid = arbiter.get_topology().await;
    assert_eq!(topo_mid.planes[0].available_memory_bytes, 0);

    // Revoking preempted lease must NOT double-reclaim memory
    arbiter.revoke_lease(batch.id).await.unwrap();
    let topo_after_revoke = arbiter.get_topology().await;
    assert_eq!(topo_after_revoke.planes[0].available_memory_bytes, 0);
    assert_eq!(arbiter.get_lease(batch.id).await.unwrap().state, LeaseState::Revoked);

    // Releasing interactive lease restores memory up to total_bytes exactly
    arbiter.release_lease(interactive.id).await.unwrap();
    let topo_final = arbiter.get_topology().await;
    assert_eq!(topo_final.planes[0].available_memory_bytes, total_bytes);
}

#[tokio::test]
async fn test_preempt_release_preempted_lease_expires_without_double_reclaim() {
    let total_bytes = 4 * 1024 * 1024 * 1024;
    let arbiter = make_test_arbiter(total_bytes);

    let batch = arbiter
        .acquire_lease(LeasePriority::Batch, total_bytes, None, None, None)
        .await
        .unwrap();

    let interactive = arbiter
        .acquire_lease(LeasePriority::Interactive, total_bytes, None, None, None)
        .await
        .unwrap();

    assert_eq!(arbiter.get_lease(batch.id).await.unwrap().state, LeaseState::Preempted);

    // Releasing preempted lease must transition to Expired without double-reclaim
    arbiter.release_lease(batch.id).await.unwrap();
    let lease_after = arbiter.get_lease(batch.id).await.unwrap();
    assert_eq!(lease_after.state, LeaseState::Expired);
    let topo_mid = arbiter.get_topology().await;
    assert_eq!(topo_mid.planes[0].available_memory_bytes, 0);

    // Releasing interactive lease restores plane memory exactly
    arbiter.release_lease(interactive.id).await.unwrap();
    let topo_final = arbiter.get_topology().await;
    assert_eq!(topo_final.planes[0].available_memory_bytes, total_bytes);
}
