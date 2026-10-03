use inferenced_core::{
    arbiter::Arbiter,
    freezer::{freeze_cgroup, is_cgroup_frozen, thaw_cgroup},
    lease::{LeasePriority, LeaseState},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use std::io::Write;
use tempfile::tempdir;
use tokio::time::{sleep, Duration};

#[tokio::test]
async fn test_scenario_multitenant_arbitration_and_cooperative_yield() {
    // Scenario 5: Multi-tenant arbitration with cooperative yield & cgroup freezing fallback
    let mut topo = HardwareTopology::default();
    topo.planes.push(
        ComputePlane::builder("plane-shared-apu")
            .name("Shared UMA APU Plane")
            .kind(ComputePlaneKind::IntegratedUma)
            .no_device_path()
            .total_memory(8 * 1024 * 1024 * 1024)
            .build(),
    );

    let arbiter = Arbiter::new(topo);

    // 1. Tenant A (Batch training job): allocates 6GB
    let tenant_a = arbiter
        .acquire_lease(
            LeasePriority::Batch,
            6 * 1024 * 1024 * 1024,
            None,
            Some("tenant-a-batch.slice".into()),
            Some(1001),
        )
        .await
        .unwrap();

    assert_eq!(tenant_a.state, LeaseState::Active);

    // 2. Tenant B (Interactive web query): arrives requiring 4GB
    // Total required (6GB + 4GB = 10GB) exceeds 8GB capacity.
    // Arbiter emits cooperative yield to Tenant A.
    arbiter.yield_lease(tenant_a.id).await.unwrap();
    let yielding_state = arbiter.get_lease(tenant_a.id).await.unwrap();
    assert_eq!(yielding_state.state, LeaseState::Preempting);

    // 3. Mock cgroup directory for fallback freezing
    let cgroup_dir = tempdir().unwrap();
    let freeze_file_path = cgroup_dir.path().join("cgroup.freeze");
    {
        let mut f = std::fs::File::create(&freeze_file_path).unwrap();
        f.write_all(b"0\n").unwrap();
    }

    // 4. Cooperative yield grace window: wait 250ms deadline
    let yield_timeout = Duration::from_millis(250);
    sleep(yield_timeout).await;

    // Tenant A did not voluntarily exit within 250ms window -> fallback to cgroup freezing
    freeze_cgroup(&freeze_file_path).expect("cgroup freeze fallback");
    arbiter.freeze_lease(tenant_a.id).await.unwrap();

    assert!(is_cgroup_frozen(&freeze_file_path).unwrap());
    assert_eq!(
        arbiter.get_lease(tenant_a.id).await.unwrap().state,
        LeaseState::Frozen
    );

    // 5. Tenant B successfully acquires compute lease while Tenant A is suspended
    let tenant_b = arbiter
        .acquire_lease(
            LeasePriority::Interactive,
            2 * 1024 * 1024 * 1024,
            None,
            Some("tenant-b-interactive.service".into()),
            Some(2002),
        )
        .await
        .unwrap();

    assert_eq!(tenant_b.state, LeaseState::Active);

    // 6. Tenant B finishes and releases lease
    arbiter.release_lease(tenant_b.id).await.unwrap();

    // 7. Tenant A is thawed and resumed
    thaw_cgroup(&freeze_file_path).expect("cgroup thaw resume");
    arbiter.thaw_lease(tenant_a.id).await.unwrap();

    assert!(!is_cgroup_frozen(&freeze_file_path).unwrap());
    assert_eq!(
        arbiter.get_lease(tenant_a.id).await.unwrap().state,
        LeaseState::Active
    );
}
