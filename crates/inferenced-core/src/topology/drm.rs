use super::types::{ComputePlane, ComputePlaneKind};
use std::fs;
use std::path::Path;

/// Compute PCIe link throughput in bytes per second from sysfs link speed and width.
/// BW = lanes * GT/s * factor (with encoding efficiency: 8b/10b for <=5GT/s, 128b/130b for >=8GT/s)
pub fn compute_pcie_bandwidth(speed_str: &str, width_str: &str) -> u64 {
    let lanes: u64 = width_str.trim().trim_start_matches('x').parse().unwrap_or(0);
    if lanes == 0 { return 0; }
    let s = speed_str.split_whitespace().next().unwrap_or("");
    let s_clean = s.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.');
    let speed_val = s_clean.parse::<f64>().unwrap_or(0.0);
    if speed_val <= 0.0 { return 0; }
    let factor = if speed_val <= 5.0 {
        0.8 / 8.0 // 8b/10b encoding = 0.10 bytes/transfer
    } else if speed_val <= 32.0 {
        (128.0 / 130.0) / 8.0 // 128b/130b encoding = ~0.1230769 bytes/transfer
    } else {
        (242.0 / 256.0) / 8.0 // PCIe 6.0 FLIT mode
    };
    (lanes as f64 * (speed_val * 1_000_000_000.0) * factor) as u64
}

/// Parse PCI resource bars from a sysfs resource file to find maximum memory aperture (VRAM).
pub fn parse_pci_resource_bars(resource_path: &Path) -> u64 {
    let content = match fs::read_to_string(resource_path) {
        Ok(c) => c,
        Err(_) => return 0,
    };
    let mut max_bar_bytes: u64 = 0;
    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let start = u64::from_str_radix(parts[0].trim_start_matches("0x"), 16).unwrap_or(0);
            let end = u64::from_str_radix(parts[1].trim_start_matches("0x"), 16).unwrap_or(0);
            if end > start {
                max_bar_bytes = max_bar_bytes.max(end - start + 1);
            }
        }
    }
    max_bar_bytes
}

/// Parse NVIDIA proprietary GPU information from /proc/driver/nvidia/gpus/*/information.
pub fn parse_nvidia_gpu_info(info_content: &str) -> (Option<String>, Option<String>) {
    let mut model = None;
    let mut bus = None;
    for line in info_content.lines() {
        if let Some((k, v)) = line.split_once(':') {
            match k.trim() {
                "Model" => model = Some(v.trim().to_string()),
                "Bus Location" => bus = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    (model, bus)
}

pub fn discover_drm_planes(dri_dir: &str, sysfs_drm_dir: &str, total_ram: u64, avail_ram: u64) -> Vec<ComputePlane> {
    let mut planes = Vec::new();
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

                    planes.push(ComputePlane {
                        id: format!("drm-{}", fname),
                        name: format!("{} ({})", vendor_name, fname),
                        kind,
                        device_path: Some(dev_path),
                        total_memory_bytes: vram_total,
                        available_memory_bytes: avail_vram,
                        numa_node: numa,
                        supported_formats: vec!["FP16".into(), "BF16".into(), "FP8".into(), "INT4".into(), "GGUF".into()],
                        is_triage_reserved: false,
                        is_quarantined: false,
                        hardware_features: features,
                        p2p_links: None,
                        kernel_used_memory: vram_used,
                    });
                }
            }
        }
    }

    // Inspect NVIDIA proprietary driver gpus directory
    let nvidia_proc = Path::new("/proc/driver/nvidia/gpus");
    if nvidia_proc.exists() {
        if let Ok(entries) = fs::read_dir(nvidia_proc) {
            for entry in entries.flatten() {
                let info_path = entry.path().join("information");
                if let Ok(info_str) = fs::read_to_string(&info_path) {
                    let (model_opt, bus_opt) = parse_nvidia_gpu_info(&info_str);
                    if let Some(bus) = bus_opt {
                        let sys_dev = Path::new("/sys/bus/pci/devices").join(&bus);
                        let bar_vram = parse_pci_resource_bars(&sys_dev.join("resource"));
                        let mut updated = false;
                        for p in &mut planes {
                            let matches_bus = p.hardware_features.iter().any(|f| f == &format!("pci-bus-{}", bus))
                                || p.id.contains(&bus);
                            if matches_bus {
                                if let Some(ref m) = model_opt {
                                    p.name = m.clone();
                                }
                                if p.total_memory_bytes < bar_vram && bar_vram > 0 {
                                    p.total_memory_bytes = bar_vram;
                                    p.available_memory_bytes = bar_vram.saturating_sub(p.kernel_used_memory);
                                }
                                updated = true;
                                break;
                            }
                        }
                        if !updated && bar_vram > 0 {
                            let speed = fs::read_to_string(sys_dev.join("current_link_speed")).unwrap_or_default();
                            let width = fs::read_to_string(sys_dev.join("current_link_width")).unwrap_or_default();
                            let bw = compute_pcie_bandwidth(&speed, &width);
                            let mut feats = vec!["drm-gem".into(), "vram-managed".into(), "nvidia-proprietary".into(), format!("pci-bus-{}", bus)];
                            if bw > 0 {
                                feats.push(format!("pcie-bw-{}", bw));
                            }
                            planes.push(ComputePlane {
                                id: format!("nvidia-{}", bus),
                                name: model_opt.unwrap_or_else(|| "NVIDIA GPU".into()),
                                kind: ComputePlaneKind::DiscreteGpu,
                                device_path: Some(sys_dev),
                                total_memory_bytes: bar_vram,
                                available_memory_bytes: bar_vram,
                                numa_node: None,
                                supported_formats: vec!["FP16".into(), "BF16".into(), "FP8".into(), "INT4".into(), "GGUF".into()],
                                is_triage_reserved: false,
                                is_quarantined: false,
                                hardware_features: feats,
                                p2p_links: None,
                                kernel_used_memory: 0,
                            });
                        }
                    }
                }
            }
        }
    }

    planes
}

pub fn inspect_drm_sysfs(sysfs_card: &Path, total_system_ram: u64) -> (String, bool, u64, u64, u64) {
    let device_dir = sysfs_card.join("device");
    let vendor = fs::read_to_string(device_dir.join("vendor"))
        .map(|v| v.trim().to_lowercase())
        .unwrap_or_default();

    let vendor_name = match vendor.as_str() {
        "0x10de" => "NVIDIA Corporation",
        "0x1002" => "Advanced Micro Devices [AMD/ATI]",
        "0x8086" => "Intel Corporation",
        "0x17cb" => "Qualcomm Technologies",
        _ => "Direct Rendering Accelerator",
    };

    let vram_used = fs::read_to_string(device_dir.join("mem_info_vram_used"))
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);

    let speed_str = fs::read_to_string(device_dir.join("current_link_speed")).unwrap_or_default();
    let width_str = fs::read_to_string(device_dir.join("current_link_width")).unwrap_or_default();
    let pcie_bw = compute_pcie_bandwidth(&speed_str, &width_str);

    let vram_path = device_dir.join("mem_info_vram_total");
    if let Ok(vram_str) = fs::read_to_string(vram_path) {
        if let Ok(vram_bytes) = vram_str.trim().parse::<u64>() {
            if vram_bytes > 0 {
                return (vendor_name.into(), false, vram_bytes, vram_used, pcie_bw);
            }
        }
    }

    let bar_vram = parse_pci_resource_bars(&device_dir.join("resource"));
    if bar_vram > 0 && vendor == "0x10de" {
        return (vendor_name.into(), false, bar_vram, vram_used, pcie_bw);
    }

    let is_integrated = vendor == "0x8086" || vendor.is_empty();
    let uma_slice = (total_system_ram / 2).max(1024 * 1024 * 1024);
    (vendor_name.into(), is_integrated, uma_slice, vram_used, pcie_bw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_pcie_bandwidth() {
        // Gen4 x16 -> ~31.5 GB/s
        let bw_gen4_x16 = compute_pcie_bandwidth("16.0 GT/s PCIe", "16");
        assert!(bw_gen4_x16 > 30_000_000_000 && bw_gen4_x16 < 33_000_000_000);

        // Gen3 x16 -> ~15.75 GB/s
        let bw_gen3_x16 = compute_pcie_bandwidth("8.0 GT/s", "16");
        assert!(bw_gen3_x16 > 15_000_000_000 && bw_gen3_x16 < 17_000_000_000);

        // Gen1 x1 -> ~250 MB/s
        let bw_gen1_x1 = compute_pcie_bandwidth("2.5 GT/s", "1");
        assert_eq!(bw_gen1_x1, 250_000_000);
        assert!(compute_pcie_bandwidth("16.0GT/s", "x16") > 30_000_000_000);
    }

    #[test]
    fn test_parse_nvidia_gpu_info() {
        let sample = "Model:\t\t NVIDIA RTX A6000\nBus Location:\t 0000:01:00.0\nIRQ:\t 42\n";
        let (model, bus) = parse_nvidia_gpu_info(sample);
        assert_eq!(model.as_deref(), Some("NVIDIA RTX A6000"));
        assert_eq!(bus.as_deref(), Some("0000:01:00.0"));
    }
}
