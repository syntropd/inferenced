pub mod affinity;
pub mod cpu;
pub mod drm;
pub mod npu;
pub mod pstore;
pub mod triage;
pub mod types;

pub use affinity::{gang_affinity_score, numa_distance, pcie_hop_distance, topology_distance};
pub use types::{ComputePlane, ComputePlaneKind, DeviceLink, HardwareTopology, LinkType};

use crate::error::Result;

impl HardwareTopology {
    /// Discover heterogeneous hardware topology across the host Linux system.
    pub fn discover() -> Result<Self> {
        let (total_ram, avail_ram) = cpu::read_meminfo_from("/proc/meminfo");
        let cpu_cores = cpu::count_cpu_cores("/proc/cpuinfo");
        let numa_nodes = cpu::count_numa_nodes("/sys/devices/system/node");

        let mut planes = Vec::new();

        // 1. Discover NPUs (/dev/accel/*, Hailo)
        let mut npus = npu::discover_npu_planes("/dev/accel", "/dev/hailo0");
        planes.append(&mut npus);

        // 2. Discover DRM Graphics & Compute Devices (/dev/dri/renderD*)
        let mut drms = drm::discover_drm_planes("/dev/dri", "/sys/class/drm", total_ram, avail_ram);
        planes.append(&mut drms);

        // 3. Discover Host CPU Matrix Plane
        let cpu_plane = cpu::build_cpu_plane("/proc/cpuinfo", avail_ram);
        planes.push(cpu_plane);

        // 4. Ingest systemd-pstore crash records to quarantine unstable hardware
        let pstore_audit = pstore::PstoreAudit::read_default();
        for plane in &mut planes {
            if pstore_audit.quarantined_drivers.iter().any(|d| {
                plane.id.to_lowercase().contains(d) || plane.name.to_lowercase().contains(d)
            }) {
                plane.is_quarantined = true;
            }
        }

        // 5. Assign Sentry Emergency Triage Enclave (skips quarantined planes)
        triage::assign_triage_enclave(&mut planes);

        // 6. Populate multi-GPU P2P link topology
        affinity::populate_p2p_links(&mut planes);

        Ok(Self {
            planes,
            total_system_ram_bytes: total_ram,
            available_system_ram_bytes: avail_ram,
            cpu_cores_total: cpu_cores,
            numa_nodes,
        })
    }
}
