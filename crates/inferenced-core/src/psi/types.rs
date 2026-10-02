//! Pressure Stall Information and telemetry types.

use serde::{Deserialize, Serialize};

tokio::task_local! {
    pub static SIMULATED_PSI: PressureLevel;
}

/// Pressure classification level derived from Linux kernel PSI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PressureLevel {
    #[default]
    Normal,
    Elevated,
    Critical,
}

/// Real-time kernel pressure stall metrics and eBPF runqueue latency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PressureMetrics {
    pub memory_some_avg10: f32,
    pub memory_full_avg10: f32,
    pub cpu_some_avg10: f32,
    pub io_some_avg10: f32,
    pub runqueue_latency_us: u64,
    pub ebpf_active: bool,
    pub level: PressureLevel,
}

impl Default for PressureMetrics {
    fn default() -> Self {
        Self {
            memory_some_avg10: 0.0,
            memory_full_avg10: 0.0,
            cpu_some_avg10: 0.0,
            io_some_avg10: 0.0,
            runqueue_latency_us: 0,
            ebpf_active: false,
            level: PressureLevel::Normal,
        }
    }
}

impl PressureMetrics {
    pub fn from_level(level: PressureLevel) -> Self {
        match level {
            PressureLevel::Critical => Self {
                memory_some_avg10: 60.0,
                memory_full_avg10: 25.0,
                cpu_some_avg10: 10.0,
                io_some_avg10: 5.0,
                runqueue_latency_us: 85_000,
                ebpf_active: false,
                level,
            },
            PressureLevel::Elevated => Self {
                memory_some_avg10: 20.0,
                memory_full_avg10: 0.0,
                cpu_some_avg10: 10.0,
                io_some_avg10: 5.0,
                runqueue_latency_us: 35_000,
                ebpf_active: false,
                level,
            },
            PressureLevel::Normal => Self {
                memory_some_avg10: 0.0,
                memory_full_avg10: 0.0,
                cpu_some_avg10: 0.0,
                io_some_avg10: 0.0,
                runqueue_latency_us: 100,
                ebpf_active: false,
                level,
            },
        }
    }

    /// Read real-time Linux kernel Pressure Stall Information (PSI).
    pub fn read_current() -> Self {
        super::collector::read_current()
    }
}
