//! NVIDIA System Management Interface (nvidia-smi) detection and querying.

/// Normalize a PCI bus ID (e.g. "00000000:01:00.0" or "0000:01:00.0") into standard 4-hex-digit domain "0000:01:00.0".
pub fn normalize_pci_bus(bus: &str) -> String {
    let clean = bus.trim().to_ascii_lowercase();
    let parts: Vec<&str> = clean.split(':').collect();
    if parts.len() == 3 {
        if let Ok(domain_num) = u32::from_str_radix(parts[0], 16) {
            return format!("{:04x}:{}:{}", domain_num, parts[1], parts[2]);
        }
    }
    clean
}

/// Information extracted from nvidia-smi.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvidiaSmiGpuInfo {
    pub bus_id: String,
    pub memory_bytes: u64,
    pub name: Option<String>,
    pub uuid: Option<String>,
}

/// Parse csv output from nvidia-smi query.
pub fn parse_nvidia_smi_output(output: &str) -> Vec<NvidiaSmiGpuInfo> {
    let mut results = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        if parts.len() >= 2 {
            let bus = normalize_pci_bus(parts[0]);
            let mib = match parts[1].parse::<u64>() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let memory_bytes = mib.saturating_mul(1024 * 1024);
            let name = if parts.len() >= 3 && !parts[2].is_empty() {
                Some(parts[2].to_string())
            } else {
                None
            };
            let uuid = if parts.len() >= 4 && !parts[3].is_empty() {
                Some(parts[3].to_string())
            } else {
                None
            };
            results.push(NvidiaSmiGpuInfo {
                bus_id: bus,
                memory_bytes,
                name,
                uuid,
            });
        }
    }
    results
}

/// Query nvidia-smi for all installed NVIDIA GPUs.
pub fn query_nvidia_smi() -> Vec<NvidiaSmiGpuInfo> {
    // Try 4-parameter query first (bus, memory, name, uuid)
    if let Ok(out) = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=pci.bus_id,memory.total,name,uuid",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            let res = parse_nvidia_smi_output(&s);
            if !res.is_empty() {
                return res;
            }
        }
    }

    // Fall back to 2-parameter query (bus, memory)
    if let Ok(out) = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=pci.bus_id,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            return parse_nvidia_smi_output(&s);
        }
    }

    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_pci_bus() {
        assert_eq!(normalize_pci_bus("00000000:01:00.0"), "0000:01:00.0");
        assert_eq!(normalize_pci_bus("0000:01:00.0"), "0000:01:00.0");
        assert_eq!(normalize_pci_bus("00000000:06:00.0"), "0000:06:00.0");
        assert_eq!(normalize_pci_bus("0000:06:00.0"), "0000:06:00.0");
        assert_eq!(normalize_pci_bus("custom-id"), "custom-id");
    }

    #[test]
    fn test_parse_nvidia_smi_output() {
        let sample_4col = "00000000:01:00.0, 16380, NVIDIA GeForce RTX 4060 Ti, GPU-aab47135\n\
                           00000000:06:00.0, 16380, NVIDIA GeForce RTX 4060 Ti, GPU-79afada8\n";
        let gpus = parse_nvidia_smi_output(sample_4col);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].bus_id, "0000:01:00.0");
        assert_eq!(gpus[0].memory_bytes, 16380 * 1024 * 1024);
        assert_eq!(gpus[0].name.as_deref(), Some("NVIDIA GeForce RTX 4060 Ti"));
        assert_eq!(gpus[0].uuid.as_deref(), Some("GPU-aab47135"));
        assert_eq!(gpus[1].bus_id, "0000:06:00.0");
        assert_eq!(gpus[1].memory_bytes, 16380 * 1024 * 1024);

        let sample_2col = "00000000:01:00.0, 16380\n";
        let gpus2 = parse_nvidia_smi_output(sample_2col);
        assert_eq!(gpus2.len(), 1);
        assert_eq!(gpus2[0].bus_id, "0000:01:00.0");
        assert_eq!(gpus2[0].memory_bytes, 16380 * 1024 * 1024);
        assert!(gpus2[0].name.is_none());
    }
}
