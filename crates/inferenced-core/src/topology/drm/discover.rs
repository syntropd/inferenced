//! DRM plane discovery across DRI nodes, NVIDIA sysfs/proc, and nvidia-smi.

use super::nvidia_proc::scan_nvidia_proc;
use super::nvidia_smi::query_nvidia_smi;
use super::sysfs::{compute_pcie_bandwidth, inspect_drm_sysfs};
use crate::topology::types::{AcceleratorCapabilities, ComputePlane, ComputePlaneKind};
use std::fs;
use std::path::Path;

pub fn discover_drm_planes(
    dri_dir: &str,
    sysfs_drm_dir: &str,
    total_ram: u64,
    avail_ram: u64,
) -> Vec<ComputePlane> {
    let mut planes = Vec::new();
    let smi_infos = query_nvidia_smi();
    let dri_path = Path::new(dri_dir);
    if dri_path.exists() {
        if let Ok(entries) = fs::read_dir(dri_path) {
            for entry in entries.flatten() {
                let fname = entry.file_name().to_string_lossy().to_string();
                if fname.starts_with("renderD") {
                    let dev_path = entry.path();
                    let sysfs_card = Path::new(sysfs_drm_dir).join(&fname);
                    let (vendor_name, is_integrated, vram_total, vram_used, pcie_bw) =
                        inspect_drm_sysfs(&sysfs_card, total_ram);

                    let kind = if is_integrated {
                        ComputePlaneKind::IntegratedUma
                    } else {
                        ComputePlaneKind::DiscreteGpu
                    };

                    let avail_vram = if is_integrated {
                        avail_ram.min(vram_total).saturating_sub(vram_used)
                    } else {
                        vram_total.saturating_sub(vram_used)
                    };

                    let numa = fs::read_to_string(sysfs_card.join("device/numa_node"))
                        .ok()
                        .and_then(|s| s.trim().parse::<i32>().ok())
                        .filter(|&n| n >= 0)
                        .map(|n| n as u32);

                    let mut features = vec!["drm-gem".into(), "vram-managed".into()];
                    if pcie_bw > 0 {
                        features.push(format!("pcie-bw-{}", pcie_bw));
                    }
                    if let Ok(canon) = fs::canonicalize(sysfs_card.join("device")) {
                        if let Some(bus_id) = canon.file_name().and_then(|n| n.to_str()) {
                            features.push(format!("pci-bus-{}", bus_id));
                        }
                    }

                    let caps = if vendor_name.contains("AMD") {
                        features.extend(["rocm".into(), "hip".into(), "amdgpu".into(), "vulkan".into(), "vulkan-compute".into()]);
                        if is_integrated { features.push("apu-unified-memory".into()); }
                        Some(AcceleratorCapabilities {
                            backend: if is_integrated { "rocm-apu".into() } else { "rocm".into() },
                            api_version: Some("6.2".into()),
                            compute_units: None,
                            structured_features: vec!["rocm".into(), "hip".into(), "vulkan".into(), "matrix-cores".into()],
                            supports_cooperative_matrix: true,
                        })
                    } else if vendor_name.contains("Intel") {
                        features.extend(["level-zero".into(), "oneapi".into(), "intel-arc".into(), "vulkan".into(), "vulkan-compute".into()]);
                        Some(AcceleratorCapabilities {
                            backend: "level-zero".into(),
                            api_version: Some("1.3".into()),
                            compute_units: None,
                            structured_features: vec!["level-zero".into(), "oneapi".into(), "vulkan".into(), "xmx".into()],
                            supports_cooperative_matrix: true,
                        })
                    } else if vendor_name.contains("NVIDIA") {
                        features.extend(["cuda".into(), "vulkan".into(), "vulkan-compute".into()]);
                        Some(AcceleratorCapabilities {
                            backend: "cuda".into(),
                            api_version: Some("12.0".into()),
                            compute_units: None,
                            structured_features: vec!["cuda".into(), "tensor-cores".into(), "vulkan".into()],
                            supports_cooperative_matrix: true,
                        })
                    } else {
                        features.extend(["vulkan".into(), "vulkan-compute".into()]);
                        Some(AcceleratorCapabilities {
                            backend: "vulkan".into(),
                            api_version: Some("1.3".into()),
                            compute_units: None,
                            structured_features: vec!["vulkan".into(), "vulkan-spirv".into()],
                            supports_cooperative_matrix: false,
                        })
                    };

                    planes.push(ComputePlane {
                        id: format!("drm-{}", fname),
                        name: format!("{} ({})", vendor_name, fname),
                        kind,
                        device_path: Some(dev_path),
                        total_memory_bytes: vram_total,
                        available_memory_bytes: avail_vram,
                        numa_node: numa,
                        supported_formats: vec![
                            "FP16".into(),
                            "BF16".into(),
                            "FP8".into(),
                            "INT4".into(),
                            "GGUF".into(),
                        ],
                        is_triage_reserved: false,
                        is_quarantined: false,
                        hardware_features: features,
                        p2p_links: None,
                        kernel_used_memory: vram_used,
                        accelerator_capabilities: caps,
                    });
                }
            }
        }
    }

    // Inspect NVIDIA proprietary driver gpus directory
    let nvidia_proc = Path::new("/proc/driver/nvidia/gpus");
    scan_nvidia_proc(nvidia_proc, &smi_infos, &mut planes);

    // Cross-check all planes with smi_infos if any plane is NVIDIA or has <= 256 MiB VRAM
    for p in &mut planes {
        for smi in &smi_infos {
            let matches = p
                .hardware_features
                .iter()
                .any(|f| f == &format!("pci-bus-{}", smi.bus_id))
                || p.id.contains(&smi.bus_id);

            if matches {
                if (p.total_memory_bytes <= 256 * 1024 * 1024
                    || smi.memory_bytes > p.total_memory_bytes)
                    && smi.memory_bytes > 0
                {
                    p.total_memory_bytes = smi.memory_bytes;
                    p.available_memory_bytes =
                        smi.memory_bytes.saturating_sub(p.kernel_used_memory);
                }
                if let Some(ref name) = smi.name {
                    if p.name.starts_with("NVIDIA Corporation")
                        || p.name.starts_with("Direct Rendering")
                    {
                        p.name = name.clone();
                    }
                }
                break;
            }
        }
    }

    // Include any NVIDIA GPUs found via nvidia-smi not yet registered in planes
    for smi in &smi_infos {
        let exists = planes.iter().any(|p| {
            p.hardware_features
                .iter()
                .any(|f| f == &format!("pci-bus-{}", smi.bus_id))
                || p.id.contains(&smi.bus_id)
        });
        if !exists && smi.memory_bytes > 0 {
            let sys_dev = Path::new("/sys/bus/pci/devices").join(&smi.bus_id);
            let speed = fs::read_to_string(sys_dev.join("current_link_speed")).unwrap_or_default();
            let width = fs::read_to_string(sys_dev.join("current_link_width")).unwrap_or_default();
            let bw = compute_pcie_bandwidth(&speed, &width);
            let mut feats = vec![
                "drm-gem".into(),
                "vram-managed".into(),
                "nvidia-proprietary".into(),
                format!("pci-bus-{}", smi.bus_id),
            ];
            if bw > 0 {
                feats.push(format!("pcie-bw-{}", bw));
            }
            planes.push(ComputePlane {
                id: format!("nvidia-{}", smi.bus_id),
                name: smi.name.clone().unwrap_or_else(|| "NVIDIA GPU".into()),
                kind: ComputePlaneKind::DiscreteGpu,
                device_path: if sys_dev.exists() {
                    Some(sys_dev)
                } else {
                    None
                },
                total_memory_bytes: smi.memory_bytes,
                available_memory_bytes: smi.memory_bytes,
                numa_node: None,
                supported_formats: vec![
                    "FP16".into(),
                    "BF16".into(),
                    "FP8".into(),
                    "INT4".into(),
                    "GGUF".into(),
                ],
                is_triage_reserved: false,
                is_quarantined: false,
                hardware_features: feats,
                p2p_links: None,
                kernel_used_memory: 0,
                accelerator_capabilities: Some(AcceleratorCapabilities {
                    backend: "cuda".into(),
                    api_version: Some("12.0".into()),
                    compute_units: None,
                    structured_features: vec!["cuda".into(), "nvml".into(), "vulkan".into()],
                    supports_cooperative_matrix: true,
                }),
            });
        }
    }

    planes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discover_empty_dir() {
        let planes = discover_drm_planes(
            "/nonexistent/dri",
            "/nonexistent/sysfs",
            16 * 1024 * 1024 * 1024,
            8 * 1024 * 1024 * 1024,
        );
        assert!(planes.is_empty() || planes.iter().all(|p| p.kind == ComputePlaneKind::DiscreteGpu));
    }
}
