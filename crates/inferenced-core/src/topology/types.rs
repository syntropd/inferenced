use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComputePlaneKind {
    DiscreteGpu,
    IntegratedUma,
    NpuAccelerator,
    CpuMatrixExtension,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkType {
    NVLink,
    PCIe,
    HostBridge,
}

impl std::fmt::Display for LinkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NVLink => write!(f, "NVLink"),
            Self::PCIe => write!(f, "PCIe"),
            Self::HostBridge => write!(f, "HostBridge"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceLink {
    pub peer_plane_id: String,
    pub link_type: LinkType,
    pub bandwidth_bytes_sec: u64,
    pub latency_nanos: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputePlane {
    pub id: String,
    pub name: String,
    pub kind: ComputePlaneKind,
    pub device_path: Option<PathBuf>,
    pub total_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub numa_node: Option<u32>,
    pub supported_formats: Vec<String>,
    pub is_triage_reserved: bool,
    pub is_quarantined: bool,
    pub hardware_features: Vec<String>,
    #[serde(default)]
    pub p2p_links: Option<Vec<DeviceLink>>,
    #[serde(default)]
    pub kernel_used_memory: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HardwareTopology {
    pub planes: Vec<ComputePlane>,
    pub total_system_ram_bytes: u64,
    pub available_system_ram_bytes: u64,
    pub cpu_cores_total: usize,
    pub numa_nodes: usize,
}

impl HardwareTopology {
    /// Sum of total GPU memory across all discrete GPU planes.
    pub fn discrete_gpu_vram(&self) -> u64 {
        self.planes
            .iter()
            .filter(|p| p.kind == ComputePlaneKind::DiscreteGpu)
            .map(|p| p.total_memory_bytes)
            .sum()
    }
}

/// Workload classification for pre-flight hardware compatibility gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkloadKind {
    TextDraft,
    TextPrimary,
    AudioSpeech,
    VisualDraft,
    VisualHighRes,
    VideoTemporal,
}

impl WorkloadKind {
    /// Whether this workload requires dedicated discrete GPU acceleration.
    pub fn is_heavy(&self) -> bool {
        matches!(self, Self::VisualHighRes | Self::VideoTemporal)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TextDraft => "TextDraft",
            Self::TextPrimary => "TextPrimary",
            Self::AudioSpeech => "AudioSpeech",
            Self::VisualDraft => "VisualDraft",
            Self::VisualHighRes => "VisualHighRes",
            Self::VideoTemporal => "VideoTemporal",
        }
    }
}

impl std::str::FromStr for WorkloadKind {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let norm: String = s.chars().filter(|c| c.is_alphanumeric()).collect();
        match norm.to_ascii_lowercase().as_str() {
            "textdraft" | "draft" => Ok(Self::TextDraft),
            "textprimary" | "text" | "primary" => Ok(Self::TextPrimary),
            "audiospeech" | "audio" | "speech" | "tts" => Ok(Self::AudioSpeech),
            "visualdraft" | "visual" => Ok(Self::VisualDraft),
            "visualhighres" | "highres" => Ok(Self::VisualHighRes),
            "videotemporal" | "video" => Ok(Self::VideoTemporal),
            other => Err(format!("unknown workload kind: {other}")),
        }
    }
}

impl std::fmt::Display for WorkloadKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
