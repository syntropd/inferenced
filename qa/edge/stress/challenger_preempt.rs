use inferenced_core::{
    arbiter::Arbiter,
    lease::{LeasePriority, LeaseState},
    preempt::{PreemptCoordinator, PreemptTier, DEFAULT_PREEMPT_TIMEOUT},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn make_test_arbiter() -> Arc<Arbiter> {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-challenger-preempt".into(),
        name: "Challenger Preempt Plane".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: 8 * 1024 * 1024 * 1024,
        available_memory_bytes: 8 * 1024 * 1024 * 1024,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
    });
    Arc::new(Arbiter::new(topo))
}

#[tokio::test]
async fn test_adversarial_preempt_exact_250ms_deadline_and_tier2_freeze() {
    let arbiter = make_test_arbiter();
    let coordinator = PreemptCoordinator::new(arbiter.clone());
    assert_eq!(DEFAULT_PREEMPT_TIMEOUT, Duration::from_millis(250));

    let lease = arbiter
        .acquire_lease(
            LeasePriority::Batch,
            1024 * 1024 * 1024,
            Some("plane-challenger-preempt".into()),
            Some("stubborn-client.service".into()),
            None,
        )
        .await
        .expect("Acquire lease should succeed");

    let start = Instant::now();
    coordinator.preempt_lease(lease.id).await.expect("Preemption coordinator must complete");
    let elapsed = start.elapsed();

    // Empirically verify 250ms deadline timeout before Tier-2 forced freeze
    assert!(
        elapsed >= Duration::from_millis(240),
        "Preemption coordinator must wait for 250ms deadline; elapsed: {:?}",
        elapsed
    );

    let rec = coordinator.get_record(lease.id).await.expect("Record must exist");
    assert_eq!(rec.tier, PreemptTier::Tier2ForcedFreeze);
    assert_eq!(rec.deadline_ms, 250);

    let frozen_lease = arbiter.get_lease(lease.id).await.expect("Lease must exist");
    assert_eq!(frozen_lease.state, LeaseState::Frozen);

    // Verify thaw restores Active state
    coordinator.thaw_lease(lease.id).await.expect("Thaw must succeed");
    let thawed_lease = arbiter.get_lease(lease.id).await.expect("Lease must exist");
    assert_eq!(thawed_lease.state, LeaseState::Active);
}

#[tokio::test]
async fn test_adversarial_preempt_cooperative_yield_under_50ms() {
    let arbiter = make_test_arbiter();
    let coordinator = PreemptCoordinator::new(arbiter.clone());

    let lease = arbiter
        .acquire_lease(
            LeasePriority::Batch,
            1024 * 1024 * 1024,
            Some("plane-challenger-preempt".into()),
            Some("cooperative-client.service".into()),
            None,
        )
        .await
        .unwrap();

    let lease_id = lease.id;
    let arb_clone = arbiter.clone();

    // Cooperative client yields after 35ms
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(35)).await;
        let _ = arb_clone.release_lease(lease_id).await;
    });

    let start = Instant::now();
    coordinator.preempt_lease(lease_id).await.unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_millis(150),
        "Cooperative yield must return early without waiting full 250ms deadline; elapsed: {:?}",
        elapsed
    );

    let rec = coordinator.get_record(lease_id).await.unwrap();
    assert_eq!(rec.tier, PreemptTier::Tier1Cooperative);
    assert!(rec.completed);
}

#[tokio::test]
async fn test_adversarial_preempt_real_child_process_sigstop_and_sigcont() {
    let mut child = Command::new("sh")
        .args(["-c", "trap '' USR1; exec sleep 30"])
        .spawn()
        .expect("Failed to spawn sleep child process");
    let pid = child.id();
    tokio::time::sleep(Duration::from_millis(25)).await;

    let arbiter = make_test_arbiter();
    // 80ms deadline for signal test
    let coordinator = PreemptCoordinator::with_deadline(arbiter.clone(), Duration::from_millis(80));

    let lease = arbiter
        .acquire_lease(
            LeasePriority::Batch,
            512 * 1024 * 1024,
            Some("plane-challenger-preempt".into()),
            None,
            Some(pid),
        )
        .await
        .unwrap();

    // Child does not cooperatively yield; deadline expires; coordinator sends SIGSTOP
    coordinator.preempt_lease(lease.id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;

    let status_path = format!("/proc/{}/status", pid);
    let status_content = fs::read_to_string(&status_path).unwrap_or_default();
    let is_stopped = status_content.lines().any(|l| l.starts_with("State:") && l.contains("T (stopped)"));
    assert!(is_stopped, "Child PID {} should be in stopped state T: {}", pid, status_content);

    // Thaw the lease; coordinator sends SIGCONT
    coordinator.thaw_lease(lease.id).await.unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;

    let thawed_status = fs::read_to_string(&status_path).unwrap_or_default();
    let is_resumed = thawed_status.lines().any(|l| l.starts_with("State:") && (l.contains("S (sleeping)") || l.contains("R (running)")));
    assert!(is_resumed, "Child PID {} should be resumed after thaw: {}", pid, thawed_status);

    let _ = child.kill();
    let _ = child.wait();
}
