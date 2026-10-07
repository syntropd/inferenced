//! Gang scheduler providing atomic all-or-nothing allocation and two-tier preemption.

use crate::error::{Error, Result};
use crate::freezer::{freeze_cgroup, send_cooperative_yield_signal};
use crate::lease::{
    CompositeLease, CompositeLeaseRequest, ComputeLease, GangPolicy, LeaseId, LeasePriority,
    LeaseState, PlaneRole, PlaneSliceAllocation,
};
use crate::topology::affinity::gang_affinity_score;
use crate::topology::{ComputePlane, ComputePlaneKind, HardwareTopology};
use std::collections::HashMap;
use tracing::{info, warn};

pub fn deduct_plane_memory(topology: &mut HardwareTopology, plane_id: &str, bytes: u64) {
    let is_uma = topology.planes.iter().find(|p| p.id == plane_id)
        .map(|p| p.kind == ComputePlaneKind::IntegratedUma).unwrap_or(false);
    if let Some(plane) = topology.planes.iter_mut().find(|p| p.id == plane_id) {
        plane.available_memory_bytes = plane.available_memory_bytes.saturating_sub(bytes);
    }
    if is_uma {
        if let Some(cpu) = topology.planes.iter_mut().find(|p| p.kind == ComputePlaneKind::CpuMatrixExtension) {
            cpu.available_memory_bytes = cpu.available_memory_bytes.saturating_sub(bytes);
        }
    }
}

pub fn restore_plane_memory(topology: &mut HardwareTopology, plane_id: &str, bytes: u64) {
    let is_uma = topology.planes.iter().find(|p| p.id == plane_id)
        .map(|p| p.kind == ComputePlaneKind::IntegratedUma).unwrap_or(false);
    if let Some(plane) = topology.planes.iter_mut().find(|p| p.id == plane_id) {
        plane.available_memory_bytes = (plane.available_memory_bytes + bytes).min(plane.total_memory_bytes);
    }
    if is_uma {
        if let Some(cpu) = topology.planes.iter_mut().find(|p| p.kind == ComputePlaneKind::CpuMatrixExtension) {
            cpu.available_memory_bytes = (cpu.available_memory_bytes + bytes).min(cpu.total_memory_bytes);
        }
    }
}

pub fn select_plane_for_single(
    topology: &HardwareTopology, priority: LeasePriority, preferred_plane: Option<&str>,
) -> Result<usize> {
    if let Some(pref) = preferred_plane {
        topology.planes.iter().position(|p| {
            p.id == pref
                || p.hardware_features.iter().any(|f| f.eq_ignore_ascii_case(pref))
                || p.name.to_lowercase().contains(&pref.to_lowercase())
                || p.accelerator_capabilities.as_ref().is_some_and(|c| c.backend.eq_ignore_ascii_case(pref))
        })
        .ok_or_else(|| Error::PlaneNotFound(pref.to_string()))
    } else if priority == LeasePriority::EmergencyTriage {
        topology.planes.iter().position(|p| p.is_triage_reserved)
            .or_else(|| topology.planes.iter().position(|p| p.kind == ComputePlaneKind::CpuMatrixExtension))
            .or_else(|| topology.planes.iter().enumerate().max_by_key(|(_, p)| p.total_memory_bytes).map(|(i, _)| i))
            .ok_or_else(|| Error::PlaneNotFound("No suitable compute plane for emergency triage".into()))
    } else {
        topology.planes.iter().enumerate()
            .filter(|(_, p)| !p.is_quarantined && (!p.is_triage_reserved || priority == LeasePriority::EmergencyTriage))
            .max_by_key(|(_, p)| p.available_memory_bytes)
            .map(|(idx, _)| idx)
            .ok_or_else(|| Error::PlaneNotFound("No available compute plane found".into()))
    }
}

pub fn preempt_plane_memory(
    topology: &mut HardwareTopology, leases: &mut HashMap<LeaseId, ComputeLease>,
    plane_idx: usize, required_bytes: u64, priority: LeasePriority,
) -> Result<()> {
    let plane_id = topology.planes[plane_idx].id.clone();
    if topology.planes[plane_idx].available_memory_bytes >= required_bytes {
        return Ok(());
    }

    let mut preemptable: Vec<LeaseId> = leases.values()
        .filter(|l| l.plane_id == plane_id && l.is_active() && l.priority < priority)
        .map(|l| l.id)
        .collect();
    preemptable.sort_by_key(|id| leases.get(id).map(|l| l.priority));

    let mut to_freeze = Vec::new();
    for pid in preemptable {
        if topology.planes[plane_idx].available_memory_bytes >= required_bytes {
            break;
        }
        if let Some(lease) = leases.get_mut(&pid) {
            warn!("Preempting lower-priority lease {} on plane {}", pid, plane_id);
            lease.state = LeaseState::Preempted;
            to_freeze.push((lease.client_pid, lease.client_unit.clone()));
            restore_plane_memory(topology, &plane_id, lease.allocated_memory_bytes);
        }
    }

    if topology.planes[plane_idx].available_memory_bytes < required_bytes {
        if priority != LeasePriority::EmergencyTriage {
            return Err(Error::ResourceExhaustion {
                plane: plane_id, requested_bytes: required_bytes,
                available_bytes: topology.planes[plane_idx].available_memory_bytes,
            });
        }
        warn!("Emergency triage lease on plane {} forces allocation", plane_id);
    }

    for (cpid, unit) in to_freeze {
        if let Some(pid) = cpid { let _ = send_cooperative_yield_signal(pid); }
        if let Some(ref u) = unit { let _ = freeze_cgroup(u); }
    }
    Ok(())
}

pub fn allocate_single(
    topology: &mut HardwareTopology, leases: &mut HashMap<LeaseId, ComputeLease>,
    priority: LeasePriority, required_bytes: u64, preferred_plane: Option<String>,
    client_unit: Option<String>, client_pid: Option<u32>,
) -> Result<ComputeLease> {
    let idx = select_plane_for_single(topology, priority, preferred_plane.as_deref())?;
    preempt_plane_memory(topology, leases, idx, required_bytes, priority)?;

    if leases.len() > 128 {
        leases.retain(|_, l| l.is_active() || l.state == LeaseState::Preempted);
    }

    let plane_id = topology.planes[idx].id.clone();
    deduct_plane_memory(topology, &plane_id, required_bytes);

    let lease = ComputeLease::new(plane_id.clone(), required_bytes, priority, client_unit, client_pid);
    leases.insert(lease.id, lease.clone());
    info!("Granted compute lease {} (priority: {:?}, memory: {} MB) on plane {}",
          lease.id, priority, required_bytes / (1024 * 1024), lease.plane_id);
    Ok(lease)
}

pub fn select_plane_for_role(
    topology: &HardwareTopology,
    role: PlaneRole,
    selected: &[usize],
) -> Option<usize> {
    match role {
        PlaneRole::Draft => topology.planes.iter().enumerate().position(|(i, p)| {
            !p.is_quarantined
                && !selected.contains(&i)
                && (p.id == "cpu-host" || p.id.contains("cpu") || p.kind == ComputePlaneKind::CpuMatrixExtension)
        }),
        PlaneRole::Target => topology.planes.iter().enumerate().position(|(i, p)| {
            !p.is_quarantined
                && !selected.contains(&i)
                && (p.id.contains("drm-renderD128") || p.id.contains("renderD128") || p.id.contains("gpu") || p.kind == ComputePlaneKind::DiscreteGpu || p.kind == ComputePlaneKind::IntegratedUma)
        }),
        _ => None,
    }
}

pub fn allocate_gang(
    topology: &mut HardwareTopology, leases: &mut HashMap<LeaseId, ComputeLease>,
    composite_leases: &mut HashMap<LeaseId, CompositeLease>, req: CompositeLeaseRequest,
) -> Result<CompositeLease> {
    let mut selected_indices = Vec::new();
    for slice in &req.slices {
        let idx = if let Some(ref pref) = slice.preferred_plane {
            topology.planes.iter().position(|p| &p.id == pref)
                .ok_or_else(|| Error::PlaneNotFound(pref.clone()))?
        } else if let Some(role_idx) = select_plane_for_role(topology, slice.role, &selected_indices) {
            role_idx
        } else {
            let mut candidates: Vec<usize> = topology.planes.iter().enumerate()
                .filter(|(i, p)| !p.is_quarantined && !selected_indices.contains(i))
                .map(|(i, _)| i)
                .collect();
            if candidates.is_empty() {
                candidates = (0..topology.planes.len()).filter(|i| !topology.planes[*i].is_quarantined).collect();
            }
            if candidates.is_empty() {
                return Err(Error::PlaneNotFound("No available compute planes for gang scheduling".into()));
            }
            candidates.sort_by_key(|&i| {
                let mut current_planes: Vec<&ComputePlane> = selected_indices.iter().map(|&idx| &topology.planes[idx]).collect();
                current_planes.push(&topology.planes[i]);
                (gang_affinity_score(&current_planes), u64::MAX - topology.planes[i].available_memory_bytes)
            });
            candidates[0]
        };
        selected_indices.push(idx);
    }

    // Strict affinity policy validation
    if req.policy == GangPolicy::StrictAffinity && selected_indices.len() > 1 {
        let first_numa = topology.planes[selected_indices[0]].numa_node;
        let affinity_ok = selected_indices.iter().all(|&idx| {
            topology.planes[idx].numa_node == first_numa
        });
        if !affinity_ok {
            return Err(Error::ResourceExhaustion {
                plane: "gang".into(),
                requested_bytes: req.total_required_bytes(),
                available_bytes: 0,
            });
        }
    }

    // Save state snapshot for atomic rollback on preemption/capacity failure
    let backup_topology = topology.clone();
    let backup_leases = leases.clone();

    // Aggregate required memory per plane
    let mut plane_demands: HashMap<usize, u64> = HashMap::new();
    for (slice, &idx) in req.slices.iter().zip(selected_indices.iter()) {
        *plane_demands.entry(idx).or_insert(0) += slice.required_bytes;
    }

    // Preempt memory per plane with transactional rollback
    for (&idx, &demanded_bytes) in &plane_demands {
        if let Err(e) = preempt_plane_memory(topology, leases, idx, demanded_bytes, req.priority) {
            *topology = backup_topology;
            *leases = backup_leases;
            return Err(e);
        }
    }

    let mut allocations = Vec::new();
    for (slice, &idx) in req.slices.iter().zip(selected_indices.iter()) {
        let plane = &topology.planes[idx];
        allocations.push(PlaneSliceAllocation {
            plane_id: plane.id.clone(),
            role: slice.role,
            allocated_memory_bytes: slice.required_bytes,
            numa_node: plane.numa_node,
            device_path: plane.device_path.clone(),
        });
        let plane_id = plane.id.clone();
        deduct_plane_memory(topology, &plane_id, slice.required_bytes);
    }

    let lease = CompositeLease::new(allocations, req.priority, req.policy, req.client_unit, req.client_pid);
    composite_leases.insert(lease.id, lease.clone());
    info!("Granted composite gang lease {} (slices: {}, policy: {:?})", lease.id, lease.slices.len(), lease.policy);
    Ok(lease)
}
