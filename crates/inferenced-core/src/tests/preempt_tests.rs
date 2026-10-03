use crate::arbiter::Arbiter;
use crate::lease::{LeasePriority, LeaseState};
use crate::preempt::{PreemptCoordinator, PreemptTier};
use crate::topology::{ComputePlane, ComputePlaneKind, HardwareTopology};
use std::sync::Arc;
use std::time::Duration;

fn setup_arbiter() -> Arc<Arbiter> {
    let mut topo = HardwareTopology::default();
    topo.planes.push(
        ComputePlane::builder("plane-preempt-test")
            .name("Test Preempt Plane")
            .kind(ComputePlaneKind::DiscreteGpu)
            .no_device_path()
            .total_memory(4 * 1024 * 1024 * 1024)
            .build(),
    );
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

#[tokio::test]
async fn test_slice_preempt_background_leases() {
    use crate::schedule::slice_preempt::SlicePreemptCoordinator;
    use tempfile::tempdir;
    let dir = tempdir().unwrap();
    let (user_slice_procs, proc_root) = (dir.path().join("cgroup.procs"), dir.path().join("proc"));
    std::fs::write(&user_slice_procs, "4242\n").unwrap();
    let fd_dir = proc_root.join("4242/fd");
    std::fs::create_dir_all(&fd_dir).unwrap();
    let target_dri = dir.path().join("renderD128");
    std::fs::write(&target_dri, "").unwrap();
    std::os::unix::fs::symlink(&target_dri, fd_dir.join("3")).unwrap();

    let arbiter = setup_arbiter();
    let preempt = Arc::new(PreemptCoordinator::new(arbiter.clone()));
    let bg_lease = arbiter
        .acquire_lease(
            LeasePriority::Batch,
            1024 * 1024 * 1024,
            Some("plane-preempt-test".into()),
            Some("system.slice/background-job.service".into()),
            Some(9999),
        )
        .await
        .unwrap();

    let coordinator = SlicePreemptCoordinator::with_paths(preempt, arbiter.clone(), user_slice_procs, proc_root);
    let status = coordinator.evaluate_and_preempt().await.unwrap();
    assert_eq!(status.interactive_pids, vec![4242]);
    assert_eq!(status.preempted_leases, vec![bg_lease.id]);
}

#[tokio::test]
async fn test_slice_preempt_cpu_contention_spike() {
    use crate::psi::{PressureLevel, SIMULATED_PSI};
    use crate::schedule::slice_preempt::SlicePreemptCoordinator;
    use tempfile::tempdir;
    let dir = tempdir().unwrap();
    let (user_slice_procs, proc_root) = (dir.path().join("cgroup.procs"), dir.path().join("proc"));
    std::fs::write(&user_slice_procs, "1000\n").unwrap();
    std::fs::create_dir_all(proc_root.join("1000/fd")).unwrap();

    let arbiter = setup_arbiter();
    let preempt = Arc::new(PreemptCoordinator::new(arbiter.clone()));
    let bg_lease = arbiter
        .acquire_lease(
            LeasePriority::Batch,
            1024 * 1024 * 1024,
            Some("plane-preempt-test".into()),
            Some("system.slice/batch.service".into()),
            Some(8888),
        )
        .await
        .unwrap();

    let coordinator = SlicePreemptCoordinator::with_paths(preempt, arbiter.clone(), user_slice_procs, proc_root);
    SIMULATED_PSI.scope(PressureLevel::Elevated, async {
        let status = coordinator.evaluate_and_preempt().await.unwrap();
        assert_eq!(status.preempted_leases, vec![bg_lease.id]);
    }).await;
}

#[tokio::test]
async fn test_slice_preempt_runtimed_immunity_and_batch() {
    use crate::psi::{PressureLevel, SIMULATED_PSI};
    use crate::schedule::slice_preempt::SlicePreemptCoordinator;
    use tempfile::tempdir;
    let dir = tempdir().unwrap();
    let (user_slice_procs, proc_root) = (dir.path().join("cgroup.procs"), dir.path().join("proc"));
    std::fs::write(&user_slice_procs, "5000\n").unwrap();
    let fd_dir = proc_root.join("5000/fd");
    std::fs::create_dir_all(&fd_dir).unwrap();
    let target_dri = dir.path().join("renderD128");
    std::fs::write(&target_dri, "").unwrap();
    std::os::unix::fs::symlink(&target_dri, fd_dir.join("4")).unwrap();

    let arbiter = setup_arbiter();
    let preempt = Arc::new(PreemptCoordinator::new(arbiter.clone()));
    let interactive_lease = arbiter
        .acquire_lease(
            LeasePriority::Interactive,
            1024 * 1024 * 1024,
            Some("plane-preempt-test".into()),
            Some("/system.slice/runtimed.service".into()),
            Some(7777),
        )
        .await
        .unwrap();

    let coordinator = SlicePreemptCoordinator::with_paths(preempt.clone(), arbiter.clone(), user_slice_procs.clone(), proc_root.clone());
    // Interactive runtimed lease must NEVER be preempted even during DRM activity and contention
    SIMULATED_PSI.scope(PressureLevel::Elevated, async {
        let status = coordinator.evaluate_and_preempt().await.unwrap();
        assert!(status.preempted_leases.is_empty());
    }).await;

    // Batch runtimed lease IS preempted when DRM user session is active
    let batch_lease = arbiter
        .acquire_lease(
            LeasePriority::Batch,
            1024 * 1024 * 1024,
            Some("plane-preempt-test".into()),
            Some("/system.slice/runtimed.service".into()),
            Some(7778),
        )
        .await
        .unwrap();

    SIMULATED_PSI.scope(PressureLevel::Elevated, async {
        let status = coordinator.evaluate_and_preempt().await.unwrap();
        assert_eq!(status.preempted_leases, vec![batch_lease.id]);
    }).await;
    let _ = interactive_lease;
}


