//! Pre-flight hardware compatibility enforcement for inference workloads.

use crate::error::{Error, Result};
use crate::topology::{ComputePlaneKind, HardwareTopology, WorkloadKind};

/// Pre-flight check that the requested workload is supported by the hardware envelope.
///
/// If a heavy workload (such as temporal video or high-res diffusion) is requested
/// on a node with zero discrete GPU VRAM, rejects the request with a structured
/// `Error::HardwareIncompatible`.
pub fn check_workload_compatibility(
    topology: &HardwareTopology,
    workload: WorkloadKind,
) -> Result<()> {
    if !workload.is_heavy() {
        return Ok(());
    }

    let discrete_vram: u64 = topology
        .planes
        .iter()
        .filter(|p| p.kind == ComputePlaneKind::DiscreteGpu)
        .map(|p| p.total_memory_bytes)
        .sum();

    if discrete_vram > 0 {
        return Ok(());
    }

    match workload {
        WorkloadKind::VideoTemporal => Err(Error::HardwareIncompatible {
            deficit: "zero discrete GPU VRAM detected (temporal video diffusion requires dedicated GPU VRAM with minimum 8GB)".into(),
            estimated_cpu_latency_secs: 480.0,
            suggested_alternatives: vec![
                "syn video generate --storyboard <n>".into(),
                "syn visual generate --storyboard <n>".into(),
                "syn video generate --allow-degrade".into(),
            ],
        }),
        WorkloadKind::VisualHighRes => Err(Error::HardwareIncompatible {
            deficit: "zero discrete GPU VRAM detected (high-resolution visual diffusion requires dedicated GPU VRAM with minimum 4GB)".into(),
            estimated_cpu_latency_secs: 180.0,
            suggested_alternatives: vec![
                "syn visual generate --storyboard <n>".into(),
                "syn visual generate --allow-degrade".into(),
            ],
        }),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::ComputePlane;

    #[test]
    fn test_admit_light_workloads_on_cpu_only() {
        let topo = HardwareTopology::default();
        assert!(check_workload_compatibility(&topo, WorkloadKind::TextDraft).is_ok());
        assert!(check_workload_compatibility(&topo, WorkloadKind::TextPrimary).is_ok());
        assert!(check_workload_compatibility(&topo, WorkloadKind::AudioSpeech).is_ok());
        assert!(check_workload_compatibility(&topo, WorkloadKind::VisualDraft).is_ok());
    }

    #[test]
    fn test_reject_heavy_workloads_on_zero_discrete_gpu() {
        let mut topo = HardwareTopology::default();
        topo.planes.push(ComputePlane {
            id: "drm-renderD128".into(),
            name: "Intel Iris Xe".into(),
            kind: ComputePlaneKind::IntegratedUma,
            device_path: None,
            total_memory_bytes: 8 * 1024 * 1024 * 1024,
            available_memory_bytes: 8 * 1024 * 1024 * 1024,
            numa_node: None,
            supported_formats: vec![],
            is_triage_reserved: false,
            is_quarantined: false,
            hardware_features: vec![],
            p2p_links: None,
            kernel_used_memory: 0,
        });

        let res_video = check_workload_compatibility(&topo, WorkloadKind::VideoTemporal);
        assert!(res_video.is_err());
        match res_video.unwrap_err() {
            Error::HardwareIncompatible { deficit, estimated_cpu_latency_secs, suggested_alternatives } => {
                assert!(deficit.contains("zero discrete GPU VRAM"));
                assert!(estimated_cpu_latency_secs >= 180.0);
                assert!(!suggested_alternatives.is_empty());
            }
            other => panic!("Expected HardwareIncompatible, got: {:?}", other),
        }

        let res_highres = check_workload_compatibility(&topo, WorkloadKind::VisualHighRes);
        assert!(res_highres.is_err());
    }

    #[test]
    fn test_admit_heavy_workloads_with_discrete_gpu() {
        let mut topo = HardwareTopology::default();
        topo.planes.push(ComputePlane {
            id: "nvidia-gpu0".into(),
            name: "RTX 4090".into(),
            kind: ComputePlaneKind::DiscreteGpu,
            device_path: None,
            total_memory_bytes: 24 * 1024 * 1024 * 1024,
            available_memory_bytes: 24 * 1024 * 1024 * 1024,
            numa_node: None,
            supported_formats: vec![],
            is_triage_reserved: false,
            is_quarantined: false,
            hardware_features: vec![],
            p2p_links: None,
            kernel_used_memory: 0,
        });

        assert!(check_workload_compatibility(&topo, WorkloadKind::VideoTemporal).is_ok());
        assert!(check_workload_compatibility(&topo, WorkloadKind::VisualHighRes).is_ok());
    }
}
