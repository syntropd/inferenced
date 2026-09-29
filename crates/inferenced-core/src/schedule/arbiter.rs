use crate::error::{Error, Result};
use crate::lease::{
    CompositeLease, CompositeLeaseRequest, ComputeLease, LeaseId, LeasePriority,
    LeaseState,
};
use crate::model::{ModelDescriptor, ModelRegistry};
use crate::psi::{PressureLevel, PressureMetrics};
use crate::schedule::gang_scheduler;
use crate::topology::{ComputePlaneKind, HardwareTopology};
use std::collections::HashMap;
use tokio::sync::RwLock;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct ArbiterState {
    pub topology: HardwareTopology,
    pub leases: HashMap<LeaseId, ComputeLease>,
    pub composite_leases: HashMap<LeaseId, CompositeLease>,
    pub registry: ModelRegistry,
}

pub struct Arbiter {
    state: RwLock<ArbiterState>,
}

impl Arbiter {
    pub fn new(topology: HardwareTopology) -> Self {
        Self {
            state: RwLock::new(ArbiterState {
                topology,
                leases: HashMap::new(),
                composite_leases: HashMap::new(),
                registry: ModelRegistry::new(),
            }),
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
    pub async fn list_composite_leases(&self) -> Vec<CompositeLease> { self.state.read().await.composite_leases.values().cloned().collect() }
    pub async fn get_composite_lease(&self, id: LeaseId) -> Option<CompositeLease> { self.state.read().await.composite_leases.get(&id).cloned() }

    pub async fn register_model(&self, desc: ModelDescriptor) { self.state.write().await.registry.register(desc); }
    pub async fn list_models(&self) -> Vec<ModelDescriptor> { self.state.read().await.registry.list() }
    pub async fn get_model(&self, id: &str) -> Option<ModelDescriptor> { self.state.read().await.registry.get(id).cloned() }
    pub async fn remove_model(&self, id: &str) -> Option<ModelDescriptor> { self.state.write().await.registry.remove(id) }
    pub async fn pin_model_for_triage(&self, id: &str, plane_id: String) -> bool { self.state.write().await.registry.pin_for_triage(id, plane_id) }

    fn check_psi_throttle(priority: LeasePriority) -> Result<()> {
        let psi = PressureMetrics::read_current();
        if psi.level == PressureLevel::Critical && priority != LeasePriority::EmergencyTriage {
            return Err(Error::BusSaturation(format!(
                "Memory bus saturated (PSI mem_some: {:.2}%, mem_full: {:.2}%). Throttling.",
                psi.memory_some_avg10, psi.memory_full_avg10
            )));
        }
        Ok(())
    }

    /// Acquire a compute slice lease. Handles preemption and Sentry emergency bypass.
    pub async fn acquire_lease(
        &self, priority: LeasePriority, required_bytes: u64, preferred_plane: Option<String>,
        client_unit: Option<String>, client_pid: Option<u32>,
    ) -> Result<ComputeLease> {
        Self::check_psi_throttle(priority)?;
        let mut state = self.state.write().await;
        let ArbiterState { ref mut topology, ref mut leases, .. } = *state;
        gang_scheduler::allocate_single(topology, leases, priority, required_bytes, preferred_plane, client_unit, client_pid)
    }

    /// Acquire a composite gang lease across multiple planes atomically.
    pub async fn acquire_composite_lease(&self, req: CompositeLeaseRequest) -> Result<CompositeLease> {
        Self::check_psi_throttle(req.priority)?;
        let mut state = self.state.write().await;
        let ArbiterState { ref mut topology, ref mut leases, ref mut composite_leases, .. } = *state;
        gang_scheduler::allocate_gang(topology, leases, composite_leases, req)
    }

    pub async fn request_lease(&self, req: LeaseRequest) -> Result<LeaseGrant> {
        self.acquire_lease(req.priority, req.required_bytes, req.preferred_plane, req.client_unit, req.client_pid).await
    }

    pub async fn health_check(&self) -> bool {
        tokio::time::timeout(std::time::Duration::from_millis(500), async {
            self.state.read().await.topology.planes.len()
        }).await.is_ok()
    }

    pub async fn release_lease(&self, lease_id: LeaseId) -> Result<()> {
        let mut state = self.state.write().await;
        let ArbiterState { ref mut topology, ref mut leases, .. } = *state;
        let lease = leases.get_mut(&lease_id).ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;
        if !lease.is_active() && lease.state != LeaseState::Preempted { return Ok(()); }
        let was_active = lease.is_active();
        lease.state = LeaseState::Expired;
        if was_active {
            gang_scheduler::restore_plane_memory(topology, &lease.plane_id, lease.allocated_memory_bytes);
        }
        info!("Released compute lease {} on plane {}", lease_id, lease.plane_id);
        Ok(())
    }

    pub async fn release_composite_lease(&self, lease_id: LeaseId) -> Result<()> {
        let mut state = self.state.write().await;
        let ArbiterState { ref mut topology, ref mut composite_leases, .. } = *state;
        let lease = composite_leases.get_mut(&lease_id).ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;
        if !lease.is_active() && lease.state != LeaseState::Preempted { return Ok(()); }
        let was_active = lease.is_active();
        lease.state = LeaseState::Expired;
        if was_active {
            for slice in &lease.slices {
                gang_scheduler::restore_plane_memory(topology, &slice.plane_id, slice.allocated_memory_bytes);
            }
        }
        info!("Released composite lease {}", lease_id);
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
            gang_scheduler::restore_plane_memory(topology, &lease.plane_id, lease.allocated_memory_bytes);
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
            gang_scheduler::deduct_plane_memory(topology, &lease.plane_id, req);
            lease.state = LeaseState::Active;
        } else if matches!(lease.state, LeaseState::Frozen | LeaseState::Preempting) {
            lease.state = LeaseState::Active;
        }
        Ok(())
    }
}

pub use crate::lease::LeaseRequest;
pub type LeaseGrant = ComputeLease;
pub type LeaseError = Error;
