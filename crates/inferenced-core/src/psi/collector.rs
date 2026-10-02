//! Real-time zero-allocation kernel telemetry and PSI collector.

use super::ebpf::collect_kernel_telemetry;
use super::stack_reader::StackPsiReader;
use super::types::{PressureLevel, PressureMetrics, SIMULATED_PSI};
use std::path::Path;
use std::sync::OnceLock;

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

    let level = if mem.full_avg10 > 10.0
        || mem.some_avg10 > 40.0
        || io.some_avg10 > 50.0
        || kernel.runqueue_latency_us > 100_000
    {
        PressureLevel::Critical
    } else if mem.some_avg10 > 15.0
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
