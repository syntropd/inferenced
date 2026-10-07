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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceleratorCapabilities {
    pub backend: String,
    pub api_version: Option<String>,
    pub compute_units: Option<u32>,
    pub structured_features: Vec<String>,
    pub supports_cooperative_matrix: bool,
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
    #[serde(default)]
    pub accelerator_capabilities: Option<AcceleratorCapabilities>,
}

impl ComputePlane {
    pub fn builder(id: impl Into<String>) -> ComputePlaneBuilder {
        ComputePlaneBuilder::new(id)
    }
}

/// Fixture builder for [`ComputePlane`] providing sensible defaults.
#[derive(Debug, Clone)]
pub struct ComputePlaneBuilder {
    id: String,
    name: Option<String>,
    kind: ComputePlaneKind,
    device_path: Option<PathBuf>,
    total_memory_bytes: u64,
    available_memory_bytes: Option<u64>,
    numa_node: Option<u32>,
    supported_formats: Vec<String>,
    is_triage_reserved: bool,
    is_quarantined: bool,
    hardware_features: Vec<String>,
    p2p_links: Option<Vec<DeviceLink>>,
    kernel_used_memory: u64,
    accelerator_capabilities: Option<AcceleratorCapabilities>,
}

impl ComputePlaneBuilder {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            kind: ComputePlaneKind::DiscreteGpu,
            device_path: Some(PathBuf::from("/dev/dri/renderD128")),
            total_memory_bytes: 24 * 1024 * 1024 * 1024,
            available_memory_bytes: None,
            numa_node: Some(0),
            supported_formats: vec!["gguf".to_string(), "safetensors".to_string()],
            is_triage_reserved: false,
            is_quarantined: false,
            hardware_features: vec!["cuda".to_string(), "flash-attn".to_string()],
            p2p_links: None,
            kernel_used_memory: 0,
            accelerator_capabilities: None,
        }
    }

    pub fn name(mut self, name: impl Into<String>) -> Self { self.name = Some(name.into()); self }
    pub fn kind(mut self, kind: ComputePlaneKind) -> Self { self.kind = kind; self }
    pub fn device_path(mut self, path: impl Into<PathBuf>) -> Self { self.device_path = Some(path.into()); self }
    pub fn no_device_path(mut self) -> Self { self.device_path = None; self }
    pub fn total_memory(mut self, bytes: u64) -> Self { self.total_memory_bytes = bytes; self }
    pub fn available_memory(mut self, bytes: u64) -> Self { self.available_memory_bytes = Some(bytes); self }
    pub fn numa_node(mut self, node: Option<u32>) -> Self { self.numa_node = node; self }
    pub fn supported_formats(mut self, formats: Vec<String>) -> Self { self.supported_formats = formats; self }
    pub fn is_triage_reserved(mut self, reserved: bool) -> Self { self.is_triage_reserved = reserved; self }
    pub fn is_quarantined(mut self, quarantined: bool) -> Self { self.is_quarantined = quarantined; self }
    pub fn hardware_features(mut self, features: Vec<String>) -> Self { self.hardware_features = features; self }
    pub fn p2p_links(mut self, links: Option<Vec<DeviceLink>>) -> Self { self.p2p_links = links; self }
    pub fn kernel_used_memory(mut self, bytes: u64) -> Self { self.kernel_used_memory = bytes; self }
    pub fn accelerator_capabilities(mut self, caps: AcceleratorCapabilities) -> Self {
        self.accelerator_capabilities = Some(caps);
        self
    }

    pub fn build(self) -> ComputePlane {
        let name = self.name.unwrap_or_else(|| format!("Plane {}", self.id));
        let total = self.total_memory_bytes;
        let avail = self.available_memory_bytes.unwrap_or(total);
        ComputePlane {
            id: self.id,
            name,
            kind: self.kind,
            device_path: self.device_path,
            total_memory_bytes: total,
            available_memory_bytes: avail,
            numa_node: self.numa_node,
            supported_formats: self.supported_formats,
            is_triage_reserved: self.is_triage_reserved,
            is_quarantined: self.is_quarantined,
            hardware_features: self.hardware_features,
            p2p_links: self.p2p_links,
            kernel_used_memory: self.kernel_used_memory,
            accelerator_capabilities: self.accelerator_capabilities,
        }
    }
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
