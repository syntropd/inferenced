use crate::error::{Error, Result};
use crate::lease::{ComputeLease, LeaseId, LeasePriority, LeaseState};
use crate::model::{ModelDescriptor, ModelRegistry};
use crate::psi::PressureMetrics;
use crate::topology::{ComputePlaneKind, HardwareTopology};
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct ArbiterState {
    pub topology: HardwareTopology,
    pub leases: HashMap<LeaseId, ComputeLease>,
    pub registry: ModelRegistry,
}

pub struct Arbiter {
    state: RwLock<ArbiterState>,
}

impl Arbiter {
    pub fn new(topology: HardwareTopology) -> Self {
        Self {
            state: RwLock::new(ArbiterState { topology, leases: HashMap::new(), registry: ModelRegistry::new() }),
        }
    }

    pub async fn get_topology(&self) -> HardwareTopology { self.state.read().await.topology.clone() }
    pub async fn update_topology(&self, mut topology: HardwareTopology) {
        let mut state = self.state.write().await;
        for lease in state.leases.values_mut() {
            if lease.is_active() {
                if let Some(plane) = topology.planes.iter_mut().find(|p| p.id == lease.plane_id) {
                    let is_uma = plane.kind == ComputePlaneKind::IntegratedUma;
                    plane.available_memory_bytes = plane.available_memory_bytes.saturating_sub(lease.allocated_memory_bytes);
                    if is_uma {
                        if let Some(cpu) = topology.planes.iter_mut().find(|p| p.kind == ComputePlaneKind::CpuMatrixExtension) {
                            cpu.available_memory_bytes = cpu.available_memory_bytes.saturating_sub(lease.allocated_memory_bytes);
                        }
                    }
                } else {
                    warn!("Plane {} hot-unplugged; revoking active lease {}", lease.plane_id, lease.id);
                    lease.state = LeaseState::Revoked;
                }
            } else if lease.state == LeaseState::Preempted && !topology.planes.iter().any(|p| p.id == lease.plane_id) {
                lease.state = LeaseState::Revoked;
            }
        }
        state.topology = topology;
    }
    pub async fn refresh_topology(&self) -> Result<()> {
        let topo = HardwareTopology::discover()?;
        self.update_topology(topo).await;
        Ok(())
    }
    pub async fn get_registry(&self) -> ModelRegistry { self.state.read().await.registry.clone() }
    pub async fn list_leases(&self) -> Vec<ComputeLease> { self.state.read().await.leases.values().cloned().collect() }
    pub async fn get_lease(&self, lease_id: LeaseId) -> Option<ComputeLease> { self.state.read().await.leases.get(&lease_id).cloned() }
    pub async fn register_model(&self, desc: ModelDescriptor) { self.state.write().await.registry.register(desc); }
    pub async fn list_models(&self) -> Vec<ModelDescriptor> { self.state.read().await.registry.list() }
    pub async fn get_model(&self, id: &str) -> Option<ModelDescriptor> { self.state.read().await.registry.get(id).cloned() }
    pub async fn remove_model(&self, id: &str) -> Option<ModelDescriptor> { self.state.write().await.registry.remove(id) }
    pub async fn pin_model_for_triage(&self, id: &str, plane_id: String) -> bool { self.state.write().await.registry.pin_for_triage(id, plane_id) }

    /// Acquire a compute slice lease. Handles preemption and Sentry emergency bypass.
    pub async fn acquire_lease(
        &self, priority: LeasePriority, required_bytes: u64, preferred_plane: Option<String>,
        client_unit: Option<String>, client_pid: Option<u32>,
    ) -> Result<ComputeLease> {
        let psi = PressureMetrics::read_current();
        if psi.level == crate::psi::PressureLevel::Critical && priority != LeasePriority::EmergencyTriage {
            return Err(Error::BusSaturation(format!(
                "Memory bus saturated (PSI mem_some: {:.2}%, mem_full: {:.2}%). Throttling.",
                psi.memory_some_avg10, psi.memory_full_avg10
            )));
        }

        let mut state = self.state.write().await;
        let target_plane_idx = if let Some(ref pref) = preferred_plane {
            state.topology.planes.iter().position(|p| &p.id == pref)
                .ok_or_else(|| Error::PlaneNotFound(pref.clone()))?
        } else if priority == LeasePriority::EmergencyTriage {
            state.topology.planes.iter().position(|p| p.is_triage_reserved)
                .or_else(|| state.topology.planes.iter().position(|p| p.kind == ComputePlaneKind::CpuMatrixExtension))
                .or_else(|| state.topology.planes.iter().enumerate().max_by_key(|(_, p)| p.total_memory_bytes).map(|(i, _)| i))
                .ok_or_else(|| Error::PlaneNotFound("No suitable compute plane for emergency triage".into()))?
        } else {
            state.topology.planes.iter().enumerate()
                .filter(|(_, p)| !p.is_quarantined && (!p.is_triage_reserved || priority == LeasePriority::EmergencyTriage))
                .max_by_key(|(_, p)| p.available_memory_bytes)
                .map(|(idx, _)| idx)
                .ok_or_else(|| Error::PlaneNotFound("No available compute plane found".into()))?
        };

        let target_plane_id = state.topology.planes[target_plane_idx].id.clone();
        if state.topology.planes[target_plane_idx].available_memory_bytes < required_bytes {
            let mut preemptable: Vec<LeaseId> = state.leases.values()
                .filter(|l| l.plane_id == target_plane_id && l.is_active() && l.priority < priority)
                .map(|l| l.id)
                .collect();
            preemptable.sort_by_key(|id| state.leases.get(id).map(|l| l.priority));

            let mut to_freeze = Vec::new();
            let ArbiterState { ref mut topology, ref mut leases, .. } = *state;
            for pid in preemptable {
                if topology.planes[target_plane_idx].available_memory_bytes >= required_bytes { break; }
                if let Some(lease) = leases.get_mut(&pid) {
                    warn!("Preempting lower-priority lease {} on plane {}", pid, target_plane_id);
                    lease.state = LeaseState::Preempted;
                    to_freeze.push((lease.client_pid, lease.client_unit.clone()));
                    restore_plane_memory(topology, &target_plane_id, lease.allocated_memory_bytes);
                }
            }

            if state.topology.planes[target_plane_idx].available_memory_bytes < required_bytes {
                if priority != LeasePriority::EmergencyTriage {
                    return Err(Error::ResourceExhaustion {
                        plane: target_plane_id, requested_bytes: required_bytes,
                        available_bytes: state.topology.planes[target_plane_idx].available_memory_bytes,
                    });
                }
                warn!("Emergency triage lease on plane {} forces allocation", target_plane_id);
            }

            for (cpid, unit) in to_freeze {
                if let Some(pid) = cpid { let _ = crate::freezer::send_cooperative_yield_signal(pid); }
                if let Some(ref u) = unit { let _ = crate::freezer::freeze_cgroup(u); }
            }
        }

        if state.leases.len() > 128 {
            state.leases.retain(|_, l| l.is_active() || l.state == LeaseState::Preempted);
        }

        let is_uma = state.topology.planes[target_plane_idx].kind == ComputePlaneKind::IntegratedUma;
        state.topology.planes[target_plane_idx].available_memory_bytes =
            state.topology.planes[target_plane_idx].available_memory_bytes.saturating_sub(required_bytes);
        if is_uma {
            if let Some(cpu) = state.topology.planes.iter_mut().find(|p| p.kind == ComputePlaneKind::CpuMatrixExtension) {
                cpu.available_memory_bytes = cpu.available_memory_bytes.saturating_sub(required_bytes);
            }
        }

        let lease = ComputeLease::new(target_plane_id.clone(), required_bytes, priority, client_unit, client_pid);
        state.leases.insert(lease.id, lease.clone());
        info!("Granted compute lease {} (priority: {:?}, memory: {} MB) on plane {}", lease.id, priority, required_bytes / (1024 * 1024), lease.plane_id);
        Ok(lease)
    }

    pub async fn request_lease(&self, req: LeaseRequest) -> Result<LeaseGrant> {
        self.acquire_lease(req.priority, req.required_bytes, req.preferred_plane, req.client_unit, req.client_pid).await
    }

    /// Health check verifying that the Arbiter state lock is responsive.
    pub async fn health_check(&self) -> bool {
        tokio::time::timeout(std::time::Duration::from_millis(500), async { self.state.read().await.topology.planes.len() }).await.is_ok()
    }

    /// Release an existing compute lease, returning memory to the plane.
    pub async fn release_lease(&self, lease_id: LeaseId) -> Result<()> {
        let mut state = self.state.write().await;
        let ArbiterState { ref mut topology, ref mut leases, .. } = *state;
        let lease = leases.get_mut(&lease_id).ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;
        if !lease.is_active() && lease.state != LeaseState::Preempted { return Ok(()); }
        let was_active = lease.is_active();
        lease.state = LeaseState::Expired;
        if was_active {
            restore_plane_memory(topology, &lease.plane_id, lease.allocated_memory_bytes);
        }
        info!("Released compute lease {} on plane {}", lease_id, lease.plane_id);
        Ok(())
    }

    pub async fn revoke_lease(&self, lease_id: LeaseId) -> Result<()> {
        let mut state = self.state.write().await;
        let ArbiterState { ref mut topology, ref mut leases, .. } = *state;
        let lease = leases.get_mut(&lease_id).ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;
        if !lease.is_active() && lease.state != LeaseState::Preempted { return Ok(()); }
        let was_active = lease.is_active();
        lease.state = LeaseState::Revoked;
        if was_active {
            restore_plane_memory(topology, &lease.plane_id, lease.allocated_memory_bytes);
        }
        info!("Revoked compute lease {} on plane {}", lease_id, lease.plane_id);
        Ok(())
    }

    pub async fn yield_lease(&self, lease_id: LeaseId) -> Result<()> {
        let mut state = self.state.write().await;
        let lease = state.leases.get_mut(&lease_id).ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;
        if !lease.is_active() { return Ok(()); }
        lease.state = LeaseState::Preempting;
        Ok(())
    }

    pub async fn freeze_lease(&self, lease_id: LeaseId) -> Result<()> {
        let mut state = self.state.write().await;
        let lease = state.leases.get_mut(&lease_id).ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;
        if !lease.is_active() { return Ok(()); }
        lease.state = LeaseState::Frozen;
        Ok(())
    }

    pub async fn thaw_lease(&self, lease_id: LeaseId) -> Result<()> {
        let mut state = self.state.write().await;
        let ArbiterState { ref mut topology, ref mut leases, .. } = *state;
        let lease = leases.get_mut(&lease_id).ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;
        if lease.state == LeaseState::Preempted {
            let req = lease.allocated_memory_bytes;
            let is_uma = topology.planes.iter().find(|p| p.id == lease.plane_id).map(|p| p.kind == ComputePlaneKind::IntegratedUma).unwrap_or(false);
            let plane = topology.planes.iter().find(|p| p.id == lease.plane_id).ok_or_else(|| Error::PlaneNotFound(lease.plane_id.clone()))?;
            if plane.available_memory_bytes < req {
                return Err(Error::ResourceExhaustion { plane: lease.plane_id.clone(), requested_bytes: req, available_bytes: plane.available_memory_bytes });
            }
            if is_uma {
                if let Some(cpu) = topology.planes.iter().find(|p| p.kind == ComputePlaneKind::CpuMatrixExtension) {
                    if cpu.available_memory_bytes < req {
                        return Err(Error::ResourceExhaustion { plane: cpu.id.clone(), requested_bytes: req, available_bytes: cpu.available_memory_bytes });
                    }
                }
            }
            if let Some(p) = topology.planes.iter_mut().find(|p| p.id == lease.plane_id) { p.available_memory_bytes = p.available_memory_bytes.saturating_sub(req); }
            if is_uma {
                if let Some(cpu) = topology.planes.iter_mut().find(|p| p.kind == ComputePlaneKind::CpuMatrixExtension) {
                    cpu.available_memory_bytes = cpu.available_memory_bytes.saturating_sub(req);
                }
            }
            lease.state = LeaseState::Active;
        } else if matches!(lease.state, LeaseState::Frozen | LeaseState::Preempting) {
            lease.state = LeaseState::Active;
        }
        Ok(())
    }
}

fn restore_plane_memory(topology: &mut HardwareTopology, plane_id: &str, bytes: u64) {
    let is_uma = topology.planes.iter().find(|p| p.id == plane_id).map(|p| p.kind == ComputePlaneKind::IntegratedUma).unwrap_or(false);
    if let Some(plane) = topology.planes.iter_mut().find(|p| p.id == plane_id) {
        plane.available_memory_bytes = (plane.available_memory_bytes + bytes).min(plane.total_memory_bytes);
    }
    if is_uma {
        if let Some(cpu) = topology.planes.iter_mut().find(|p| p.kind == ComputePlaneKind::CpuMatrixExtension) {
            cpu.available_memory_bytes = (cpu.available_memory_bytes + bytes).min(cpu.total_memory_bytes);
        }
    }
}

pub use crate::lease::LeaseRequest;
pub type LeaseGrant = ComputeLease;
pub type LeaseError = Error;

