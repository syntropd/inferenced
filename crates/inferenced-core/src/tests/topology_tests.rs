use crate::topology::{
    cpu, drm, npu, triage, ComputePlane, ComputePlaneKind, HardwareTopology,
};
use std::fs;
use tempfile::tempdir;

#[test]
fn test_topology_discover_live() {
    let topo = HardwareTopology::discover().unwrap();
    assert!(!topo.planes.is_empty(), "Should discover at least CPU plane");
    let has_cpu = topo.planes.iter().any(|p| p.kind == ComputePlaneKind::CpuMatrixExtension);
    assert!(has_cpu, "Must include host CPU matrix plane");
    assert!(topo.total_system_ram_bytes > 0);
}

#[test]
fn test_drm_sysfs_parsing_mock() {
    let dir = tempdir().unwrap();
    let dev_dir = dir.path().join("device");
    fs::create_dir_all(&dev_dir).unwrap();
    fs::write(dev_dir.join("vendor"), "0x10de\n").unwrap();
    fs::write(dev_dir.join("mem_info_vram_total"), "8589934592\n").unwrap();

    let (vendor, integrated, vram, _used, _bw) = drm::inspect_drm_sysfs(dir.path(), 16 * 1024 * 1024 * 1024);
    assert_eq!(vendor, "NVIDIA Corporation");
    assert!(!integrated);
    assert_eq!(vram, 8589934592);
}

#[test]
fn test_npu_discovery_mock() {
    let dir = tempdir().unwrap();
    let accel_dir = dir.path().join("accel");
    fs::create_dir_all(accel_dir.join("accel0")).unwrap();
    let hailo_file = dir.path().join("hailo0");
    fs::write(&hailo_file, "").unwrap();

    let planes = npu::discover_npu_planes(
        accel_dir.to_str().unwrap(),
        hailo_file.to_str().unwrap(),
    );
    assert_eq!(planes.len(), 2);
    assert_eq!(planes[0].kind, ComputePlaneKind::NpuAccelerator);
    assert_eq!(planes[1].kind, ComputePlaneKind::NpuAccelerator);
}

#[test]
fn test_cpu_discovery_mock() {
    let dir = tempdir().unwrap();
    let cpuinfo_path = dir.path().join("cpuinfo");
    let content = "processor : 0\nflags : fpu amx_tile avx512_vnni avx2\nprocessor : 1\nflags : fpu amx_tile\n";
    fs::write(&cpuinfo_path, content).unwrap();

    let (has_amx, has_vnni, has_avx2) = cpu::detect_cpu_features(cpuinfo_path.to_str().unwrap());
    assert!(has_amx);
    assert!(has_vnni);
    assert!(has_avx2);

    let cores = cpu::count_cpu_cores(cpuinfo_path.to_str().unwrap());
    assert_eq!(cores, 2);

    let plane = cpu::build_cpu_plane(cpuinfo_path.to_str().unwrap(), 16 * 1024 * 1024 * 1024);
    assert_eq!(plane.kind, ComputePlaneKind::CpuMatrixExtension);
    assert!(plane.hardware_features.contains(&"Intel-AMX".to_string()));
    assert!(plane.hardware_features.contains(&"AVX512-VNNI".to_string()));
}

#[test]
fn test_triage_enclave_assignment_and_quota() {
    let mut planes = vec![
        ComputePlane {
            id: "cpu-host".into(),
            name: "Host CPU".into(),
            kind: ComputePlaneKind::CpuMatrixExtension,
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
    ];

    let assigned = triage::assign_triage_enclave(&mut planes);
    assert_eq!(assigned, Some("cpu-host".into()));
    // Reserves 2GB quota on CPU fallback without locking out general workloads
    assert!(!planes[0].is_triage_reserved);
    assert_eq!(planes[0].available_memory_bytes, 6 * 1024 * 1024 * 1024);
}
