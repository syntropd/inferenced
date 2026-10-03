use inferenced_core::topology::{
    cpu, triage, ComputePlane, ComputePlaneKind, HardwareTopology,
};
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn test_discovery_read_meminfo_valid() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "MemTotal:       32768 kB").unwrap();
    writeln!(file, "MemFree:         8192 kB").unwrap();
    writeln!(file, "MemAvailable:   16384 kB").unwrap();

    let (total, avail) = cpu::read_meminfo_from(file.path().to_str().unwrap());
    assert_eq!(total, 32768 * 1024);
    assert_eq!(avail, 16384 * 1024);
}

#[test]
fn test_discovery_detect_cpu_features_matrix() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "flags: fpu vme amx_tile avx512_vnni avx2").unwrap();

    let (amx, vnni, avx2) = cpu::detect_cpu_features(file.path().to_str().unwrap());
    assert!(amx, "AMX tile should be detected");
    assert!(vnni, "AVX512 VNNI should be detected");
    assert!(avx2, "AVX2 should be detected");
}

#[test]
fn test_discovery_count_cpu_cores() {
    let mut file = NamedTempFile::new().unwrap();
    for i in 0..8 {
        writeln!(file, "processor : {}", i).unwrap();
    }

    let count = cpu::count_cpu_cores(file.path().to_str().unwrap());
    assert_eq!(count, 8);
}

#[test]
fn test_discovery_build_cpu_plane() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "processor : 0\nflags: amx_tile avx512_vnni avx2").unwrap();

    let plane = cpu::build_cpu_plane(file.path().to_str().unwrap(), 32 * 1024 * 1024 * 1024);
    assert_eq!(plane.kind, ComputePlaneKind::CpuMatrixExtension);
    assert_eq!(plane.id, "cpu-host");
    assert!(plane.hardware_features.contains(&"Intel-AMX".to_string()));
    assert!(plane.hardware_features.contains(&"AVX512-VNNI".to_string()));
    assert!(plane.hardware_features.contains(&"AVX2-FMA".to_string()));
}

#[test]
fn test_discovery_triage_enclave_selection_hierarchy() {
    let mut planes = vec![
        ComputePlane::builder("gpu-0")
            .name("Discrete GPU")
            .kind(ComputePlaneKind::DiscreteGpu)
            .no_device_path()
            .total_memory(16 * 1024 * 1024 * 1024)
            .build(),
        ComputePlane::builder("npu-0")
            .name("Neural Processing Unit")
            .kind(ComputePlaneKind::NpuAccelerator)
            .no_device_path()
            .total_memory(4 * 1024 * 1024 * 1024)
            .build(),
    ];

    let reserved = triage::assign_triage_enclave(&mut planes);
    assert_eq!(reserved, Some("npu-0".into()));
    assert!(planes[1].is_triage_reserved);
    assert!(!planes[0].is_triage_reserved);
}

#[test]
fn test_discovery_hardware_topology_live() {
    let topo = HardwareTopology::discover().expect("Host discovery must succeed");
    assert!(!topo.planes.is_empty(), "At least host CPU plane must be discovered");
    assert!(topo.cpu_cores_total >= 1, "At least 1 CPU core must exist");
    assert!(topo.total_system_ram_bytes > 0, "System RAM must be positive");
}
