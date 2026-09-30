use inferenced_core::{
    arbiter::Arbiter,
    lease::{LeasePriority, LeaseState},
    model::{ModelDescriptor, ModelPlacementState, ModelRegistry},
    topology::{
        cpu, triage, ComputePlane, ComputePlaneKind, HardwareTopology,
    },
};
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;

mod cli_tests;
mod daemon;
mod discovery_tests;
mod fd_tests;
mod freezer_tests;
mod madvise_tests;
mod preempt_tests;
mod varlink_tests;

#[test]
fn test_parse_kb() {
    assert_eq!(cpu::parse_kb("1024 kB"), 1024 * 1024);
    assert_eq!(cpu::parse_kb("   2048 kB  "), 2048 * 1024);
    assert_eq!(cpu::parse_kb("invalid"), 0);
    assert_eq!(cpu::parse_kb(""), 0);
}

#[test]
fn test_read_meminfo_from_mock() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "MemTotal:       16384 kB").unwrap();
    writeln!(file, "MemFree:         4096 kB").unwrap();
    writeln!(file, "MemAvailable:    8192 kB").unwrap();

    let (total, avail) = cpu::read_meminfo_from(file.path().to_str().unwrap());
    assert_eq!(total, 16384 * 1024);
    assert_eq!(avail, 8192 * 1024);
}

#[test]
fn test_detect_cpu_features_mock() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "flags: fpu vme amx_tile avx512_vnni avx2").unwrap();

    let (amx, vnni, avx2) = cpu::detect_cpu_features(file.path().to_str().unwrap());
    assert!(amx);
    assert!(vnni);
    assert!(avx2);
}

#[test]
fn test_count_cpu_cores_mock() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "processor : 0").unwrap();
    writeln!(file, "processor : 1").unwrap();
    writeln!(file, "processor : 2").unwrap();
    writeln!(file, "processor : 3").unwrap();

    let count = cpu::count_cpu_cores(file.path().to_str().unwrap());
    assert_eq!(count, 4);
}

#[test]
fn test_build_cpu_plane() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "processor : 0\nflags: avx2").unwrap();

    let plane = cpu::build_cpu_plane(file.path().to_str().unwrap(), 10 * 1024 * 1024 * 1024);
    assert_eq!(plane.kind, ComputePlaneKind::CpuMatrixExtension);
    assert_eq!(plane.id, "cpu-host");
    assert!(plane.hardware_features.contains(&"AVX2-FMA".to_string()));
}

#[test]
fn test_assign_triage_enclave_prefers_npu() {
    let mut planes = vec![
        ComputePlane {
            id: "gpu-0".into(),
            name: "Discrete GPU".into(),
            kind: ComputePlaneKind::DiscreteGpu,
            device_path: None,
            total_memory_bytes: 8 * 1024 * 1024 * 1024,
            available_memory_bytes: 8 * 1024 * 1024 * 1024,
            numa_node: None,
            supported_formats: vec![],
            is_triage_reserved: false,
        is_quarantined: false,
            hardware_features: vec![],
        p2p_links: None,
        kernel_used_memory: 0,
        },
        ComputePlane {
            id: "npu-accel0".into(),
            name: "Intel NPU".into(),
            kind: ComputePlaneKind::NpuAccelerator,
            device_path: None,
            total_memory_bytes: 2 * 1024 * 1024 * 1024,
            available_memory_bytes: 2 * 1024 * 1024 * 1024,
            numa_node: None,
            supported_formats: vec![],
            is_triage_reserved: false,
        is_quarantined: false,
            hardware_features: vec![],
        p2p_links: None,
        kernel_used_memory: 0,
        },
    ];

    let reserved = triage::assign_triage_enclave(&mut planes);
    assert_eq!(reserved, Some("npu-accel0".into()));
    assert!(planes[1].is_triage_reserved);
    assert!(!planes[0].is_triage_reserved);
}

#[test]
fn test_assign_triage_enclave_falls_back_to_cpu() {
    let mut planes = vec![ComputePlane {
        id: "cpu-host".into(),
        name: "Host CPU".into(),
        kind: ComputePlaneKind::CpuMatrixExtension,
        device_path: None,
        total_memory_bytes: 16 * 1024 * 1024 * 1024,
        available_memory_bytes: 16 * 1024 * 1024 * 1024,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
        p2p_links: None,
        kernel_used_memory: 0,
    }];

    let reserved = triage::assign_triage_enclave(&mut planes);
    assert_eq!(reserved, Some("cpu-host".into()));
    assert!(!planes[0].is_triage_reserved, "CPU plane must remain accessible to general workloads");
    assert_eq!(
        planes[0].available_memory_bytes,
        14 * 1024 * 1024 * 1024,
        "CPU plane must have 2GB reserved quota for emergency triage"
    );
}

#[test]
fn test_assign_triage_enclave_proportional_on_constrained_host() {
    let mut planes = vec![ComputePlane {
        id: "cpu-edge".into(),
        name: "Edge Host CPU".into(),
        kind: ComputePlaneKind::CpuMatrixExtension,
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
    }];

    let reserved = triage::assign_triage_enclave(&mut planes);
    assert_eq!(reserved, Some("cpu-edge".into()));
    assert!(!planes[0].is_triage_reserved);
    // On a 4GB system, 25% (1GB) is reserved for emergency triage, leaving 3GB for normal workloads
    assert_eq!(
        planes[0].available_memory_bytes,
        3 * 1024 * 1024 * 1024,
        "Proportional triage quota should reserve 1GB on a 4GB edge host"
    );
}


#[tokio::test]
async fn test_arbiter_acquire_and_release_lease() {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-test".into(),
        name: "Test Accelerator".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: 10 * 1024 * 1024 * 1024,
        available_memory_bytes: 10 * 1024 * 1024 * 1024,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
        p2p_links: None,
        kernel_used_memory: 0,
    });

    let arbiter = Arbiter::new(topo);
    let needed = 2 * 1024 * 1024 * 1024;

    let lease = arbiter
        .acquire_lease(LeasePriority::Interactive, needed, None, Some("test.service".into()), None)
        .await
        .unwrap();

    assert_eq!(lease.plane_id, "plane-test");
    assert_eq!(lease.state, LeaseState::Active);

    let current_topo = arbiter.get_topology().await;
    assert_eq!(
        current_topo.planes[0].available_memory_bytes,
        8 * 1024 * 1024 * 1024
    );

    // Release lease
    arbiter.release_lease(lease.id).await.unwrap();
    let restored_topo = arbiter.get_topology().await;
    assert_eq!(
        restored_topo.planes[0].available_memory_bytes,
        10 * 1024 * 1024 * 1024
    );
}

#[test]
fn test_model_registry_lifecycle() {
    let mut reg = ModelRegistry::new();
    let desc = ModelDescriptor {
        id: "qwen2.5-coder:7b".into(),
        format: "GGUF".into(),
        path: PathBuf::from("/models/qwen.gguf"),
        estimated_memory_bytes: 4 * 1024 * 1024 * 1024,
        placement: ModelPlacementState::Dormant,
        resident_plane_id: None,
        last_accessed: chrono::Utc::now(),
        access_count: 0,
        preferred_plane: None,
    };

    reg.register(desc);
    assert_eq!(reg.list().len(), 1);

    assert!(reg.pin_for_triage("qwen2.5-coder:7b", "npu-accel0".into()));
    let pinned = reg.get("qwen2.5-coder:7b").unwrap();
    assert_eq!(pinned.placement, ModelPlacementState::PinnedTriage);
    assert_eq!(pinned.resident_plane_id, Some("npu-accel0".into()));

    reg.remove("qwen2.5-coder:7b");
    assert!(reg.get("qwen2.5-coder:7b").is_none());
}
