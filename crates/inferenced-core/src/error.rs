use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Hardware topology discovery failed: {0}")]
    TopologyDiscovery(String),

    #[error("Compute plane not found: {0}")]
    PlaneNotFound(String),

    #[error("Resource exhaustion: insufficient memory on plane {plane} (requested {requested_bytes} bytes, available {available_bytes} bytes)")]
    ResourceExhaustion {
        plane: String,
        requested_bytes: u64,
        available_bytes: u64,
    },

    #[error("Memory bus saturation / Pressure stall critical: {0}")]
    BusSaturation(String),

    #[error("Hardware incompatible with requested workload: {deficit}")]
    HardwareIncompatible {
        deficit: String,
        estimated_cpu_latency_secs: f64,
        suggested_alternatives: Vec<String>,
    },

    #[error("Lease not found: {0}")]
    LeaseNotFound(String),

    #[error("Lease preemption failed: {0}")]
    PreemptionFailed(String),

    #[error("Model not found: {0}")]
    ModelNotFound(String),

    #[error("Engine communication error: {0}")]
    EngineCommunication(String),

    #[error("Zero-copy FD error: {0}")]
    Fd(String),

    #[error("Cgroup freezer error: {0}")]
    Freezer(String),

    #[error("Madvise page management error: {0}")]
    Madvise(String),

    #[error("Systemd interface error: {0}")]
    Systemd(String),

    #[error("System call error: {0}")]
    SystemCall(#[from] rustix::io::Errno),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
