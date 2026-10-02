//! Real-time zero-allocation kernel telemetry and PSI collector.

use super::ebpf::collect_kernel_telemetry;
use super::stack_reader::StackPsiReader;
use super::types::{PressureLevel, PressureMetrics, SIMULATED_PSI};
use serde::Deserialize;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

pub const TUNING_JSON_PATH: &str = "/run/syntrop/tuning.json";

/// Dynamic closed-loop tuning parameters reloaded from `/run/syntrop/tuning.json`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DynamicTuning {
    pub memory_some_threshold: f64,
    pub memory_full_threshold: f64,
    pub k_draft_horizon: usize,
    pub max_tokens_clamp: usize,
}

impl Default for DynamicTuning {
    fn default() -> Self {
        Self {
            memory_some_threshold: 25.0,
            memory_full_threshold: 10.0,
            k_draft_horizon: 4,
            max_tokens_clamp: 128,
        }
    }
}

#[derive(Deserialize)]
struct RawTuning {
    #[serde(default = "default_some")]
    memory_some_threshold: f64,
    #[serde(default = "default_full")]
    memory_full_threshold: f64,
    #[serde(default = "default_k")]
    k_draft_horizon: usize,
    #[serde(default = "default_clamp")]
    max_tokens_clamp: usize,
}

fn default_some() -> f64 { 25.0 }
fn default_full() -> f64 { 10.0 }
fn default_k() -> usize { 4 }
fn default_clamp() -> usize { 128 }

impl RawTuning {
    fn into_dynamic(self) -> DynamicTuning {
        DynamicTuning {
            memory_some_threshold: self.memory_some_threshold,
            memory_full_threshold: self.memory_full_threshold,
            k_draft_horizon: self.k_draft_horizon,
            max_tokens_clamp: self.max_tokens_clamp,
        }
    }
}

struct CacheEntry {
    last_mtime: Option<SystemTime>,
    tuning: DynamicTuning,
}

static TUNING_CACHE: Mutex<Option<std::collections::HashMap<std::path::PathBuf, CacheEntry>>> = Mutex::new(None);

/// Poll dynamic tuning configuration from `/run/syntrop/tuning.json`.
pub fn poll_tuning_config() -> DynamicTuning {
    poll_tuning_from_path(Path::new(TUNING_JSON_PATH))
}

/// Poll dynamic tuning configuration from a custom path (allows test isolation).
pub fn poll_tuning_from_path(path: &Path) -> DynamicTuning {
    let mut guard = match TUNING_CACHE.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let map = guard.get_or_insert_with(std::collections::HashMap::new);

    let current_mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();

    if let Some(entry) = map.get_mut(path) {
        if entry.last_mtime == current_mtime {
            return entry.tuning;
        }
        if let Some(mtime) = current_mtime {
            if let Ok(content) = std::fs::read(path) {
                if let Ok(raw) = serde_json::from_slice::<RawTuning>(&content) {
                    entry.last_mtime = Some(mtime);
                    entry.tuning = raw.into_dynamic();
                    return entry.tuning;
                }
            }
            return entry.tuning;
        }
        entry.last_mtime = None;
        entry.tuning = DynamicTuning::default();
        entry.tuning
    } else {
        let mut tuning = DynamicTuning::default();
        let mut loaded_mtime = None;
        if let Some(mtime) = current_mtime {
            if let Ok(content) = std::fs::read(path) {
                if let Ok(raw) = serde_json::from_slice::<RawTuning>(&content) {
                    tuning = raw.into_dynamic();
                    loaded_mtime = Some(mtime);
                }
            }
        }
        map.insert(path.to_path_buf(), CacheEntry {
            last_mtime: loaded_mtime,
            tuning,
        });
        tuning
    }
}

static ENV_SIMULATED_PSI: OnceLock<Option<PressureLevel>> = OnceLock::new();

fn get_env_simulated_psi() -> Option<PressureLevel> {
    *ENV_SIMULATED_PSI.get_or_init(|| {
        std::env::var("INFERENCED_SIMULATE_PSI").ok().and_then(|sim| {
            let sim_lower = sim.to_ascii_lowercase();
            if sim_lower == "critical" {
                Some(PressureLevel::Critical)
            } else if sim_lower == "elevated" {
                Some(PressureLevel::Elevated)
            } else if sim_lower == "normal" {
                Some(PressureLevel::Normal)
            } else {
                None
            }
        })
    })
}

/// Reads real-time Linux kernel Pressure Stall Information (PSI) and kernel telemetry.
/// Allocates zero heap bytes during steady-state ticks.
pub fn read_current() -> PressureMetrics {
    let tuning = poll_tuning_config();

    if let Ok(sim_lvl) = SIMULATED_PSI.try_with(|lvl| *lvl) {
        return PressureMetrics::from_level(sim_lvl);
    }

    if let Some(lvl) = get_env_simulated_psi() {
        return PressureMetrics::from_level(lvl);
    }

    let mem = StackPsiReader::read_path(Path::new("/proc/pressure/memory")).unwrap_or_default();
    let cpu = StackPsiReader::read_path(Path::new("/proc/pressure/cpu")).unwrap_or_default();
    let io = StackPsiReader::read_path(Path::new("/proc/pressure/io")).unwrap_or_default();

    let kernel = collect_kernel_telemetry(cpu.some_avg10, io.some_avg10);

    let level = if mem.full_avg10 > (tuning.memory_full_threshold as f32)
        || mem.some_avg10 > (tuning.memory_some_threshold as f32)
        || io.some_avg10 > 50.0
        || kernel.runqueue_latency_us > 100_000
    {
        PressureLevel::Critical
    } else if mem.some_avg10 > (tuning.memory_some_threshold * 0.6) as f32
        || io.some_avg10 > 20.0
        || cpu.some_avg10 > 60.0
        || kernel.runqueue_latency_us > 40_000
    {
        PressureLevel::Elevated
    } else {
        PressureLevel::Normal
    };

    PressureMetrics {
        memory_some_avg10: mem.some_avg10,
        memory_full_avg10: mem.full_avg10,
        cpu_some_avg10: cpu.some_avg10,
        io_some_avg10: io.some_avg10,
        runqueue_latency_us: kernel.runqueue_latency_us,
        ebpf_active: kernel.ebpf_active,
        level,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_poll_tuning_defaults_and_reload() {
        let temp_dir = tempfile::tempdir().unwrap();
        let tuning_file = temp_dir.path().join("tuning.json");

        // Non-existent path defaults to balanced
        let default_cfg = poll_tuning_from_path(&tuning_file);
        assert_eq!(default_cfg, DynamicTuning::default());
        assert_eq!(default_cfg.memory_some_threshold, 25.0);
        assert_eq!(default_cfg.memory_full_threshold, 10.0);
        assert_eq!(default_cfg.k_draft_horizon, 4);
        assert_eq!(default_cfg.max_tokens_clamp, 128);

        // Create file with aggressive tuning
        let aggressive_json = r#"{
            "policy": "aggressive",
            "memory_some_threshold": 40.0,
            "memory_full_threshold": 20.0,
            "k_draft_horizon": 8,
            "max_tokens_clamp": 256
        }"#;
        std::fs::write(&tuning_file, aggressive_json).unwrap();

        let loaded = poll_tuning_from_path(&tuning_file);
        assert_eq!(loaded.memory_some_threshold, 40.0);
        assert_eq!(loaded.memory_full_threshold, 20.0);
        assert_eq!(loaded.k_draft_horizon, 8);
        assert_eq!(loaded.max_tokens_clamp, 256);

        // Cached call without modification
        let cached = poll_tuning_from_path(&tuning_file);
        assert_eq!(cached, loaded);

        // Malformed write retains last valid tuning and recovers on valid update
        std::fs::write(&tuning_file, "{ malformed").unwrap();
        let retained = poll_tuning_from_path(&tuning_file);
        assert_eq!(retained, loaded);
        std::fs::write(&tuning_file, aggressive_json).unwrap();
        let recovered = poll_tuning_from_path(&tuning_file);
        assert_eq!(recovered, loaded);

        // Remove file: reloads and falls back to balanced defaults
        std::fs::remove_file(&tuning_file).unwrap();
        let fallback = poll_tuning_from_path(&tuning_file);
        assert_eq!(fallback, DynamicTuning::default());

        // Alternate path isolation check
        let other_file = temp_dir.path().join("other.json");
        assert_eq!(poll_tuning_from_path(&other_file), DynamicTuning::default());
    }
}

