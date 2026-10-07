use super::types::{ComputePlane, ComputePlaneKind};
use std::fs;
use std::path::Path;

pub fn discover_npu_planes(accel_dir: &str, hailo_dev: &str) -> Vec<ComputePlane> {
    let mut planes = Vec::new();

    // Linux 6.2+ /dev/accel/*
    let accel_path = Path::new(accel_dir);
    if accel_path.exists() {
        if let Ok(entries) = fs::read_dir(accel_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("accel") {
                    planes.push(ComputePlane {
                        id: format!("npu-{}", name),
                        name: format!("Linux Compute Accelerator ({})", name),
                        kind: ComputePlaneKind::NpuAccelerator,
                        device_path: Some(path),
                        total_memory_bytes: 4 * 1024 * 1024 * 1024,
                        available_memory_bytes: 4 * 1024 * 1024 * 1024,
                        numa_node: None,
                        supported_formats: vec!["INT8".into(), "FP8".into(), "INT4".into(), "ONNX".into()],
                        is_triage_reserved: false,
                        is_quarantined: false,
                        hardware_features: vec!["low-power".into(), "sram-scratchpad".into(), "accel-subsystem".into()],
                        p2p_links: None,
                        kernel_used_memory: 0,
                        accelerator_capabilities: None,
                    });
                }
            }
        }
    }

    // Hailo AI Accelerator
    let hailo_path = Path::new(hailo_dev);
    if hailo_path.exists() {
        planes.push(ComputePlane {
            id: "npu-hailo0".into(),
            name: "Hailo AI Accelerator".into(),
            kind: ComputePlaneKind::NpuAccelerator,
            device_path: Some(hailo_path.to_path_buf()),
            total_memory_bytes: 2 * 1024 * 1024 * 1024,
            available_memory_bytes: 2 * 1024 * 1024 * 1024,
            numa_node: None,
            supported_formats: vec!["HEF".into(), "INT8".into()],
            is_triage_reserved: false,
            is_quarantined: false,
            hardware_features: vec!["26-tops".into(), "pcie-edge".into()],
            p2p_links: None,
            kernel_used_memory: 0,
            accelerator_capabilities: None,
        });
    }

    planes
}
