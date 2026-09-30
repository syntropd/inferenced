use inferenced_core::{
    arbiter::Arbiter,
    error::Error,
    lease::{LeaseId, LeasePriority},
    topology::{cpu, ComputePlane, ComputePlaneKind, HardwareTopology},
};
use std::io::Write;
use tempfile::NamedTempFile;

mod faults;
mod reclaim;
mod stress;
mod transport;


#[tokio::test]
async fn test_edge_insufficient_memory_rejection() {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-small".into(),
        name: "Small Accelerator".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: 1024 * 1024 * 1024,     // 1GB
        available_memory_bytes: 512 * 1024 * 1024, // 512MB
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
        p2p_links: None,
        kernel_used_memory: 0,
    });

    let arbiter = Arbiter::new(topo);
    // Request 2GB on a 512MB plane
    let res = arbiter
        .acquire_lease(
            LeasePriority::Interactive,
            2 * 1024 * 1024 * 1024,
            Some("plane-small".into()),
            None,
            None,
        )
        .await;

    assert!(matches!(res, Err(Error::ResourceExhaustion { .. })));
}

#[tokio::test]
async fn test_edge_nonexistent_plane_requested() {
    let topo = HardwareTopology::default();
    let arbiter = Arbiter::new(topo);

    let res = arbiter
        .acquire_lease(
            LeasePriority::Interactive,
            1024,
            Some("nonexistent-plane-404".into()),
            None,
            None,
        )
        .await;

    assert!(matches!(res, Err(Error::PlaneNotFound(_))));
}

#[tokio::test]
async fn test_edge_release_unknown_lease() {
    let topo = HardwareTopology::default();
    let arbiter = Arbiter::new(topo);

    let fake_id = LeaseId::default();
    let res = arbiter.release_lease(fake_id).await;
    assert!(matches!(res, Err(Error::LeaseNotFound(_))));
}

#[tokio::test]
async fn test_edge_sentry_emergency_preemption_succeeds_even_when_exhausted() {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "npu-triage".into(),
        name: "Dedicated NPU".into(),
        kind: ComputePlaneKind::NpuAccelerator,
        device_path: None,
        total_memory_bytes: 1024 * 1024 * 1024, // 1GB
        available_memory_bytes: 0,              // 0 bytes free!
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: true,
        is_quarantined: false,
        hardware_features: vec![],
        p2p_links: None,
        kernel_used_memory: 0,
    });

    let arbiter = Arbiter::new(topo);
    // Emergency triage must succeed even when zero memory is reported
    let res = arbiter
        .acquire_lease(
            LeasePriority::EmergencyTriage,
            512 * 1024 * 1024,
            None,
            Some("systemd-sentry.service".into()),
            None,
        )
        .await;

    assert!(res.is_ok(), "Emergency triage should preempt and succeed!");
    let lease = res.unwrap();
    assert_eq!(lease.priority, LeasePriority::EmergencyTriage);
    assert_eq!(lease.plane_id, "npu-triage");
}

#[test]
fn test_edge_corrupt_or_empty_meminfo() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "Corrupt file without MemTotal or MemAvailable").unwrap();
    let (total, avail) = cpu::read_meminfo_from(file.path().to_str().unwrap());
    assert_eq!(total, 0);
    assert_eq!(avail, 0);

    // Non-existent path
    let (total2, avail2) = cpu::read_meminfo_from("/path/that/does/not/exist");
    assert_eq!(total2, 0);
    assert_eq!(avail2, 0);
}

#[test]
fn test_edge_empty_cpuinfo_core_fallback() {
    let file = NamedTempFile::new().unwrap();
    // Empty file should safely return at least 1 core
    let cores = cpu::count_cpu_cores(file.path().to_str().unwrap());
    assert_eq!(cores, 1);
}
