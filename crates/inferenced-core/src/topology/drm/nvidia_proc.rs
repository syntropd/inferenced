//! NVIDIA proprietary driver `/proc/driver/nvidia/gpus` inspection.

use super::nvidia_smi::{normalize_pci_bus, NvidiaSmiGpuInfo};
use super::sysfs::{compute_pcie_bandwidth, parse_pci_resource_bars};
use crate::topology::types::{ComputePlane, ComputePlaneKind};
use std::fs;
use std::path::Path;

/// Parse NVIDIA proprietary GPU information from /proc/driver/nvidia/gpus/*/information.
pub fn parse_nvidia_gpu_info(info_content: &str) -> (Option<String>, Option<String>) {
    let (model, bus, _) = parse_nvidia_gpu_info_full(info_content);
    (model, bus)
}

/// Parse NVIDIA proprietary GPU information including GPU UUID.
pub fn parse_nvidia_gpu_info_full(
    info_content: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let mut model = None;
    let mut bus = None;
    let mut uuid = None;
    for line in info_content.lines() {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim() {
                "Model" => model = Some(v.trim().to_string()),
                "Bus Location" => bus = Some(v.trim().to_string()),
                "GPU UUID" => uuid = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    (model, bus, uuid)
}

/// Discovers or augments NVIDIA planes from `/proc/driver/nvidia/gpus`.
pub fn scan_nvidia_proc(
    nvidia_proc_dir: &Path,
    smi_infos: &[NvidiaSmiGpuInfo],
    planes: &mut Vec<ComputePlane>,
) {
    if !nvidia_proc_dir.exists() {
        return;
    }
    let entries = match fs::read_dir(nvidia_proc_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let info_path = entry.path().join("information");
        let info_str = match fs::read_to_string(&info_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let (model_opt, bus_opt, uuid_opt) = parse_nvidia_gpu_info_full(&info_str);
        let bus = match bus_opt {
            Some(b) => b,
            None => continue,
        };

        let norm_bus = normalize_pci_bus(&bus);
        let sys_dev = Path::new("/sys/bus/pci/devices").join(&bus);
        let bar_vram = parse_pci_resource_bars(&sys_dev.join("resource"));

        let smi_match = smi_infos.iter().find(|s| {
            s.bus_id == norm_bus || (uuid_opt.is_some() && s.uuid.as_deref() == uuid_opt.as_deref())
        });

        let effective_vram = match smi_match {
            Some(smi) if smi.memory_bytes > 0 => {
                if bar_vram <= 256 * 1024 * 1024 || smi.memory_bytes > bar_vram {
                    smi.memory_bytes
                } else {
                    bar_vram
                }
            }
            _ => bar_vram,
        };

        let effective_model = model_opt.or_else(|| smi_match.and_then(|s| s.name.clone()));

        let mut updated = false;
        for p in planes.iter_mut() {
            let matches_bus = p.hardware_features.iter().any(|f| {
                f == &format!("pci-bus-{}", bus) || f == &format!("pci-bus-{}", norm_bus)
            }) || p.id.contains(&bus)
                || p.id.contains(&norm_bus);
            if matches_bus {
                if let Some(ref m) = effective_model {
                    p.name = m.clone();
                }
                if effective_vram > 0
                    && (p.total_memory_bytes <= 256 * 1024 * 1024
                        || effective_vram > p.total_memory_bytes)
                {
                    p.total_memory_bytes = effective_vram;
                    p.available_memory_bytes =
                        effective_vram.saturating_sub(p.kernel_used_memory);
                }
                updated = true;
                break;
            }
        }

        if !updated && effective_vram > 0 {
            let speed = fs::read_to_string(sys_dev.join("current_link_speed")).unwrap_or_default();
            let width = fs::read_to_string(sys_dev.join("current_link_width")).unwrap_or_default();
            let bw = compute_pcie_bandwidth(&speed, &width);
            let mut feats = vec![
                "drm-gem".into(),
                "vram-managed".into(),
                "nvidia-proprietary".into(),
                format!("pci-bus-{}", bus),
            ];
            if bw > 0 {
                feats.push(format!("pcie-bw-{}", bw));
            }
            planes.push(ComputePlane {
                id: format!("nvidia-{}", bus),
                name: effective_model.unwrap_or_else(|| "NVIDIA GPU".into()),
                kind: ComputePlaneKind::DiscreteGpu,
                device_path: Some(sys_dev),
                total_memory_bytes: effective_vram,
                available_memory_bytes: effective_vram,
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
                accelerator_capabilities: Some(crate::topology::types::AcceleratorCapabilities {
                    backend: "cuda".into(),
                    api_version: Some("12.0".into()),
                    compute_units: None,
                    structured_features: vec!["cuda".into(), "nvml".into(), "vulkan".into()],
                    supports_cooperative_matrix: true,
                }),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_nvidia_gpu_info() {
        let sample = "Model:\t\t NVIDIA RTX A6000\nBus Location:\t 0000:01:00.0\nIRQ:\t 42\nGPU UUID:\t GPU-1234-5678\n";
        let (model, bus) = parse_nvidia_gpu_info(sample);
        assert_eq!(model.as_deref(), Some("NVIDIA RTX A6000"));
        assert_eq!(bus.as_deref(), Some("0000:01:00.0"));

        let (model2, bus2, uuid2) = parse_nvidia_gpu_info_full(sample);
        assert_eq!(model2.as_deref(), Some("NVIDIA RTX A6000"));
        assert_eq!(bus2.as_deref(), Some("0000:01:00.0"));
        assert_eq!(uuid2.as_deref(), Some("GPU-1234-5678"));
    }
}
