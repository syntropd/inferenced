//! Hardware affinity scoring based on NUMA distance and PCIe tree depth.

use super::types::ComputePlane;
use std::path::Path;

pub const NUMA_LOCAL_DISTANCE: u32 = 10;
pub const NUMA_REMOTE_DISTANCE: u32 = 20;
pub const NUMA_DEFAULT_DISTANCE: u32 = 15;

/// Compute NUMA distance between two NUMA node identifiers.
pub fn numa_distance(node_a: Option<u32>, node_b: Option<u32>) -> u32 {
    match (node_a, node_b) {
        (Some(a), Some(b)) if a == b => NUMA_LOCAL_DISTANCE,
        (Some(_), Some(_)) => NUMA_REMOTE_DISTANCE,
        _ => NUMA_DEFAULT_DISTANCE,
    }
}

/// Calculate PCIe switch hop distance between two device paths.
/// Returns estimated hop count (1 = peer ports on same switch, 2 = same host bridge, 4 = cross socket/unknown).
pub fn pcie_hop_distance(dev_a: Option<&Path>, dev_b: Option<&Path>) -> u32 {
    let (Some(pa), Some(pb)) = (dev_a, dev_b) else {
        return 4;
    };

    if pa == pb {
        return 0;
    }

    let comps_a: Vec<_> = pa.components().collect();
    let comps_b: Vec<_> = pb.components().collect();

    let mut common = 0;
    for (ca, cb) in comps_a.iter().zip(comps_b.iter()) {
        if ca == cb {
            common += 1;
        } else {
            break;
        }
    }

    let diff_a = comps_a.len().saturating_sub(common);
    let diff_b = comps_b.len().saturating_sub(common);
    let total_diff = (diff_a + diff_b) as u32;

    match total_diff {
        0 => 0,
        1 | 2 => 1,
        3 | 4 => 2,
        _ => 4,
    }
}

/// Compute topology distance between two compute planes. Lower is closer.
pub fn topology_distance(a: &ComputePlane, b: &ComputePlane) -> u32 {
    let numa = numa_distance(a.numa_node, b.numa_node);
    let pcie = pcie_hop_distance(a.device_path.as_deref(), b.device_path.as_deref());
    (numa * 2) + pcie
}

/// Compute aggregate affinity score for a gang of compute planes. Lower distance is better.
pub fn gang_affinity_score(planes: &[&ComputePlane]) -> u64 {
    if planes.len() <= 1 {
        return 0;
    }

    let mut total: u64 = 0;
    for i in 0..planes.len() {
        for j in (i + 1)..planes.len() {
            total += topology_distance(planes[i], planes[j]) as u64;
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::types::ComputePlaneKind;
    use std::path::PathBuf;

    fn mock_plane(id: &str, numa: Option<u32>, path: Option<&str>) -> ComputePlane {
        ComputePlane {
            id: id.to_string(),
            name: id.to_string(),
            kind: ComputePlaneKind::DiscreteGpu,
            device_path: path.map(PathBuf::from),
            total_memory_bytes: 16 * 1024 * 1024 * 1024,
            available_memory_bytes: 16 * 1024 * 1024 * 1024,
            numa_node: numa,
            supported_formats: vec!["FP16".into()],
            is_triage_reserved: false,
            is_quarantined: false,
            hardware_features: vec![],
        }
    }

    #[test]
    fn test_numa_distance() {
        assert_eq!(numa_distance(Some(0), Some(0)), 10);
        assert_eq!(numa_distance(Some(0), Some(1)), 20);
        assert_eq!(numa_distance(Some(0), None), 15);
    }

    #[test]
    fn test_pcie_hops_and_gang_score() {
        let p1 = mock_plane("gpu0", Some(0), Some("/sys/devices/pci0000:00/0000:00:01.0/renderD128"));
        let p2 = mock_plane("gpu1", Some(0), Some("/sys/devices/pci0000:00/0000:00:01.0/renderD129"));
        let p3 = mock_plane("gpu2", Some(1), Some("/sys/devices/pci0000:80/0000:80:01.0/renderD130"));

        let d12 = topology_distance(&p1, &p2);
        let d13 = topology_distance(&p1, &p3);
        assert!(d12 < d13, "Same NUMA/switch should have lower distance: {} vs {}", d12, d13);

        let gang_score = gang_affinity_score(&[&p1, &p2]);
        assert_eq!(gang_score, d12 as u64);
    }
}
