//! DRM device plane discovery and hardware aperture inspection.

pub mod discover;
pub mod nvidia_proc;
pub mod nvidia_smi;
pub mod sysfs;

pub use discover::discover_drm_planes;
pub use nvidia_proc::parse_nvidia_gpu_info;
pub use nvidia_proc::parse_nvidia_gpu_info_full;
pub use nvidia_proc::scan_nvidia_proc;
pub use nvidia_smi::normalize_pci_bus;
pub use nvidia_smi::parse_nvidia_smi_output;
pub use nvidia_smi::query_nvidia_smi;
pub use nvidia_smi::NvidiaSmiGpuInfo;
pub use sysfs::compute_pcie_bandwidth;
pub use sysfs::inspect_drm_sysfs;
pub use sysfs::parse_pci_resource_bars;
