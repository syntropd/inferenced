use super::types::{ComputePlane, ComputePlaneKind};
use std::fs;
use std::path::Path;

pub fn read_meminfo_from(path: &str) -> (u64, u64) {
    let Ok(content) = fs::read_to_string(path) else {
        return (0, 0);
    };
    let mut total = 0;
    let mut avail = 0;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = parse_kb(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail = parse_kb(rest);
        }
    }
    (total, avail)
}

pub fn parse_kb(rest: &str) -> u64 {
    rest.split_whitespace()
        .next()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

pub fn count_numa_nodes(sysfs_node_path: &str) -> usize {
    let path = Path::new(sysfs_node_path);
    if !path.exists() {
        return 1;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 1;
    };
    let count = entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .map(|s| s.starts_with("node") && s[4..].chars().all(|c| c.is_ascii_digit()))
                .unwrap_or(false)
        })
        .count();
    if count == 0 { 1 } else { count }
}

pub fn detect_cpu_features(cpuinfo_path: &str) -> (bool, bool, bool) {
    let Ok(content) = fs::read_to_string(cpuinfo_path) else {
        return (false, false, false);
    };
    let mut has_amx = false;
    let mut has_vnni = false;
    let mut has_avx2 = false;

    for line in content.lines() {
        if line.starts_with("flags") || line.starts_with("Features") {
            if line.contains("amx") { has_amx = true; }
            if line.contains("avx512_vnni") || line.contains("avx512vnni") { has_vnni = true; }
            if line.contains("avx2") { has_avx2 = true; }
            break;
        }
    }
    (has_amx, has_vnni, has_avx2)
}

pub fn count_cpu_cores(cpuinfo_path: &str) -> usize {
    let Ok(content) = fs::read_to_string(cpuinfo_path) else {
        return 1;
    };
    let count = content
        .lines()
        .filter(|l| l.starts_with("processor"))
        .count();
    if count == 0 { 1 } else { count }
}

pub fn build_cpu_plane(cpuinfo_path: &str, avail_ram: u64) -> ComputePlane {
    let cores = count_cpu_cores(cpuinfo_path);
    let (has_amx, has_vnni, has_avx2) = detect_cpu_features(cpuinfo_path);

    let mut features = Vec::new();
    if has_amx { features.push("Intel-AMX".into()); }
    if has_vnni { features.push("AVX512-VNNI".into()); }
    if has_avx2 { features.push("AVX2-FMA".into()); }
    features.push(format!("{}-cores", cores));

    let cpu_usable_ram = (avail_ram as f64 * 0.75) as u64;

    ComputePlane {
        id: "cpu-host".into(),
        name: "Host CPU SIMD/Matrix Plane".into(),
        kind: ComputePlaneKind::CpuMatrixExtension,
        device_path: None,
        total_memory_bytes: avail_ram,
        available_memory_bytes: cpu_usable_ram,
        numa_node: Some(0),
        supported_formats: vec!["GGUF".into(), "INT4".into(), "INT8".into(), "FP32".into()],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: features,
        p2p_links: None,
        kernel_used_memory: 0,
    }
}
