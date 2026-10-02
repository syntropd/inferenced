//! Linux Pressure Stall Information (PSI) and kernel telemetry ingestion.

pub mod collector;
pub mod ebpf;
pub mod stack_reader;
pub mod types;

pub use collector::read_current;
pub use ebpf::{collect_kernel_telemetry, probe_ebpf_privilege, KernelTelemetry};
pub use stack_reader::{StackPsiReader, StackPsiValues};
pub use types::{PressureLevel, PressureMetrics, SIMULATED_PSI};
