//! Integration tests for gang scheduling: atomic all-or-nothing, rollback, and affinity.

use crate::arbiter::Arbiter;
use crate::lease::{
    CompositeLeaseRequest, GangPolicy, LeasePriority, LeaseState, PlaneRole, SliceRequirement,
};
use crate::topology::types::{ComputePlane, ComputePlaneKind, HardwareTopology};

fn create_test_plane(id: &str, mem_gb: u64, numa: Option<u32>) -> ComputePlane {
    ComputePlane::builder(id)
        .name(id)
        .kind(ComputePlaneKind::DiscreteGpu)
        .no_device_path()
        .total_memory(mem_gb * 1024 * 1024 * 1024)
        .numa_node(numa)
        .supported_formats(vec!["FP16".into()])
        .build()
}

#[tokio::test]
async fn test_gang_same_plane_demand_aggregation_fails() {
    let mut topo = HardwareTopology::default();
    topo.planes.push(create_test_plane("gpu0", 6, Some(0)));

    let arbiter = Arbiter::new(topo);
    // Request 2 slices of 4GB on same 6GB plane.
    let gang_req = CompositeLeaseRequest {
        priority: LeasePriority::Interactive,
        slices: vec![
            SliceRequirement {
                role: PlaneRole::Primary,
                required_bytes: 4 * 1024 * 1024 * 1024,
                preferred_plane: Some("gpu0".into()),
            },
            SliceRequirement {
                role: PlaneRole::Worker,
                required_bytes: 4 * 1024 * 1024 * 1024,
                preferred_plane: Some("gpu0".into()),
            },
        ],
        policy: GangPolicy::AllOrNothing,
        client_unit: Some("test.service".into()),
        client_pid: Some(100),
    };

    let res = arbiter.acquire_composite_lease(gang_req).await;
    assert!(res.is_err(), "Aggregated 8GB on 6GB plane must fail");

    let t = arbiter.get_topology().await;
    assert_eq!(t.planes[0].available_memory_bytes, 6 * 1024 * 1024 * 1024);
}

#[tokio::test]
async fn test_gang_atomic_rollback_on_partial_failure() {
    let mut topo = HardwareTopology::default();
    topo.planes.push(create_test_plane("gpu0", 4, Some(0)));
    topo.planes.push(create_test_plane("gpu1", 2, Some(0)));

    let arbiter = Arbiter::new(topo);

    // Lease 3GB on gpu0 with Batch priority
    let l1 = arbiter
        .acquire_lease(
            LeasePriority::Batch,
            3 * 1024 * 1024 * 1024,
            Some("gpu0".into()),
            Some("batch.service".into()),
            Some(200),
        )
        .await
        .unwrap();

    // Now request gang: Interactive 4GB on gpu0 (can preempt l1), and 4GB on gpu1 (gpu1 only has 2GB!)
    let gang_req = CompositeLeaseRequest {
        priority: LeasePriority::Interactive,
        slices: vec![
            SliceRequirement {
                role: PlaneRole::Primary,
                required_bytes: 4 * 1024 * 1024 * 1024,
                preferred_plane: Some("gpu0".into()),
            },
            SliceRequirement {
                role: PlaneRole::Worker,
                required_bytes: 4 * 1024 * 1024 * 1024,
                preferred_plane: Some("gpu1".into()),
            },
        ],
        policy: GangPolicy::AllOrNothing,
        client_unit: Some("gang.service".into()),
        client_pid: Some(300),
    };

    let res = arbiter.acquire_composite_lease(gang_req).await;
    assert!(res.is_err(), "Gang must fail because gpu1 cannot satisfy 4GB");

    // Verify atomic rollback: l1 must STILL be Active on gpu0, not preempted!
    let lease_after = arbiter.get_lease(l1.id).await.unwrap();
    assert_eq!(lease_after.state, LeaseState::Active);
}

#[tokio::test]
async fn test_gang_strict_affinity_rejects_cross_numa() {
    let mut topo = HardwareTopology::default();
    topo.planes.push(create_test_plane("gpu0", 8, Some(0)));
    topo.planes.push(create_test_plane("gpu1", 8, Some(1)));

    let arbiter = Arbiter::new(topo);
    let gang_req = CompositeLeaseRequest {
        priority: LeasePriority::Interactive,
        slices: vec![
            SliceRequirement {
                role: PlaneRole::Primary,
                required_bytes: 2 * 1024 * 1024 * 1024,
                preferred_plane: Some("gpu0".into()),
            },
            SliceRequirement {
                role: PlaneRole::Worker,
                required_bytes: 2 * 1024 * 1024 * 1024,
                preferred_plane: Some("gpu1".into()),
            },
        ],
        policy: GangPolicy::StrictAffinity,
        client_unit: Some("strict.service".into()),
        client_pid: Some(400),
    };

    let res = arbiter.acquire_composite_lease(gang_req).await;
    assert!(res.is_err(), "Strict affinity across NUMA 0 and 1 must be rejected");
}
