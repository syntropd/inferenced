//! DRM sysfs telemetry reader sampling VRAM memory metrics via non-blocking fixed buffers.

use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub const DEFAULT_DRM_SYSFS_PATH: &str = "/sys/class/drm";
pub const DEFAULT_PSI_MEMORY_PATH: &str = "/proc/pressure/memory";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrmWatermark {
    pub render_device: String,
    pub vendor: String,
    pub vram_used_bytes: u64,
    pub vram_total_bytes: u64,
    pub used_ratio: f64,
    pub is_fallback_psi: bool,
}

fn read_nonblocking_buf(path: &Path) -> Option<([u8; 64], usize)> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let mut buf = [0u8; 64];
    let n = file.read(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    Some((buf, n))
}

pub fn read_nonblocking_u64(path: &Path) -> Option<u64> {
    let (buf, n) = read_nonblocking_buf(path)?;
    let s = std::str::from_utf8(&buf[..n]).ok()?.trim();
    s.parse::<u64>().ok()
}

pub fn read_nonblocking_string(path: &Path) -> Option<String> {
    let (buf, n) = read_nonblocking_buf(path)?;
    let s = std::str::from_utf8(&buf[..n]).ok()?.trim();
    Some(s.to_string())
}

pub fn read_nonblocking_psi_avg10(path: &Path) -> Option<f64> {
    let (buf, n) = read_nonblocking_buf(path)?;
    let s = std::str::from_utf8(&buf[..n]).ok()?;
    for part in s.split_whitespace() {
        if let Some(val_str) = part.strip_prefix("avg10=") {
            return val_str.parse::<f64>().ok();
        }
    }
    None
}

pub fn sample_single_device(render_dir: &Path, psi_path: &Path) -> Option<DrmWatermark> {
    let dev_name = render_dir.file_name()?.to_str()?.to_string();
    let device_dir = render_dir.join("device");
    let vendor_raw = read_nonblocking_string(&device_dir.join("vendor"))
        .unwrap_or_default()
        .to_lowercase();

    let (vendor, is_nvidia) = match vendor_raw.as_str() {
        "0x10de" => ("NVIDIA Corporation".to_string(), true),
        "0x1002" => ("Advanced Micro Devices [AMD/ATI]".to_string(), false),
        "0x8086" => ("Intel Corporation".to_string(), false),
        v if !v.is_empty() => (v.to_string(), false),
        _ => ("Generic DRM Device".to_string(), false),
    };

    // 1. AMD sysfs: mem_info_vram_used & mem_info_vram_total
    let amd_used = read_nonblocking_u64(&device_dir.join("mem_info_vram_used"));
    let amd_total = read_nonblocking_u64(&device_dir.join("mem_info_vram_total"));

    // 2. Intel Xe lmem: tile0/memory/vram0/used or lmem_used_bytes
    let intel_used = read_nonblocking_u64(&device_dir.join("tile0/memory/vram0/used"))
        .or_else(|| read_nonblocking_u64(&device_dir.join("lmem_used_bytes")));
    let intel_total = read_nonblocking_u64(&device_dir.join("tile0/memory/vram0/total"))
        .or_else(|| read_nonblocking_u64(&device_dir.join("tile0/physical_vram_size")))
        .or_else(|| read_nonblocking_u64(&device_dir.join("lmem_total_bytes")));

    if let (Some(used), Some(total)) = (amd_used.or(intel_used), amd_total.or(intel_total)) {
        let ratio = if total > 0 { (used as f64) / (total as f64) } else { 0.0 };
        return Some(DrmWatermark {
            render_device: dev_name,
            vendor,
            vram_used_bytes: used,
            vram_total_bytes: total,
            used_ratio: ratio.clamp(0.0, 1.0),
            is_fallback_psi: false,
        });
    }

    // 3. NVIDIA or non-sysfs device: PSI fallback
    let psi_avg10 = read_nonblocking_psi_avg10(psi_path).unwrap_or(0.0);
    let ratio = (psi_avg10 / 100.0).clamp(0.0, 1.0);
    let estimated_total = 16 * 1024 * 1024 * 1024u64; // 16 GiB standard accelerator quota
    let estimated_used = (estimated_total as f64 * ratio) as u64;

    Some(DrmWatermark {
        render_device: dev_name,
        vendor: if is_nvidia { "NVIDIA Corporation".to_string() } else { vendor },
        vram_used_bytes: estimated_used,
        vram_total_bytes: estimated_total,
        used_ratio: ratio,
        is_fallback_psi: true,
    })
}

pub fn sample_drm_watermarks_from(drm_base: &Path, psi_path: &Path) -> Vec<DrmWatermark> {
    let mut watermarks = Vec::new();
    let entries = match std::fs::read_dir(drm_base) {
        Ok(e) => e,
        Err(_) => return watermarks,
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("renderD") {
            if let Some(wm) = sample_single_device(&entry.path(), psi_path) {
                watermarks.push(wm);
            }
        }
    }
    watermarks.sort_by(|a, b| a.render_device.cmp(&b.render_device));
    watermarks
}

pub fn sample_drm_watermarks() -> Vec<DrmWatermark> {
    sample_drm_watermarks_from(
        Path::new(DEFAULT_DRM_SYSFS_PATH),
        Path::new(DEFAULT_PSI_MEMORY_PATH),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, write};
    use tempfile::tempdir;

    #[test]
    fn test_sample_amd_sysfs_watermark() {
        let dir = tempdir().unwrap();
        let card = dir.path().join("renderD128");
        let dev = card.join("device");
        create_dir_all(&dev).unwrap();
        write(dev.join("vendor"), "0x1002\n").unwrap();
        write(dev.join("mem_info_vram_used"), "4294967296\n").unwrap();
        write(dev.join("mem_info_vram_total"), "8589934592\n").unwrap();

        let psi_file = dir.path().join("pressure_mem");
        write(&psi_file, "some avg10=5.00 avg60=0.00 avg300=0.00 total=0\n").unwrap();

        let wm = sample_single_device(&card, &psi_file).unwrap();
        assert_eq!(wm.render_device, "renderD128");
        assert!(wm.vendor.contains("Advanced Micro Devices"));
        assert_eq!(wm.vram_used_bytes, 4294967296);
        assert_eq!(wm.vram_total_bytes, 8589934592);
        assert!((wm.used_ratio - 0.50).abs() < 1e-4);
        assert!(!wm.is_fallback_psi);
    }

    #[test]
    fn test_sample_intel_xe_watermark() {
        let dir = tempdir().unwrap();
        let card = dir.path().join("renderD129");
        let vram = card.join("device/tile0/memory/vram0");
        create_dir_all(&vram).unwrap();
        write(card.join("device/vendor"), "0x8086\n").unwrap();
        write(vram.join("used"), "3221225472\n").unwrap();
        write(vram.join("total"), "17179869184\n").unwrap();

        let psi_file = dir.path().join("pressure_mem");
        let wm = sample_single_device(&card, &psi_file).unwrap();
        assert_eq!(wm.render_device, "renderD129");
        assert_eq!(wm.vendor, "Intel Corporation");
        assert_eq!(wm.vram_used_bytes, 3221225472);
        assert_eq!(wm.vram_total_bytes, 17179869184);
        assert!(!wm.is_fallback_psi);
    }

    #[test]
    fn test_sample_nvidia_psi_fallback() {
        let dir = tempdir().unwrap();
        let card = dir.path().join("renderD130");
        create_dir_all(card.join("device")).unwrap();
        write(card.join("device/vendor"), "0x10de\n").unwrap();

        let psi_file = dir.path().join("pressure_mem");
        write(&psi_file, "some avg10=25.00 avg60=10.00 avg300=5.00 total=12345\n").unwrap();

        let wm = sample_single_device(&card, &psi_file).unwrap();
        assert_eq!(wm.render_device, "renderD130");
        assert_eq!(wm.vendor, "NVIDIA Corporation");
        assert!(wm.is_fallback_psi);
        assert!((wm.used_ratio - 0.25).abs() < 1e-4);
        assert_eq!(wm.vram_used_bytes, (wm.vram_total_bytes as f64 * 0.25) as u64);
    }

    #[test]
    fn test_sample_live_host_drm_watermarks() {
        let list = sample_drm_watermarks();
        if Path::new(DEFAULT_DRM_SYSFS_PATH).exists() {
            for wm in &list {
                assert!(wm.render_device.starts_with("renderD"));
                assert!(wm.vram_total_bytes > 0);
                assert!(wm.used_ratio >= 0.0 && wm.used_ratio <= 1.0);
            }
        }
    }
}
