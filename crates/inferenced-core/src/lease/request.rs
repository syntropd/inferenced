//! Lease acquisition request contracts.

use super::composite::{GangPolicy, PlaneRole};
use super::single::LeasePriority;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRequest {
    pub priority: LeasePriority,
    pub required_bytes: u64,
    pub preferred_plane: Option<String>,
    pub client_unit: Option<String>,
    pub client_pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SliceRequirement {
    pub role: PlaneRole,
    pub required_bytes: u64,
    pub preferred_plane: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeLeaseRequest {
    pub priority: LeasePriority,
    pub slices: Vec<SliceRequirement>,
    pub policy: GangPolicy,
    pub client_unit: Option<String>,
    pub client_pid: Option<u32>,
}

impl CompositeLeaseRequest {
    pub fn new(
        slices: Vec<SliceRequirement>,
        priority: LeasePriority,
        policy: GangPolicy,
        client_unit: Option<String>,
        client_pid: Option<u32>,
    ) -> Self {
        Self {
            priority,
            slices,
            policy,
            client_unit,
            client_pid,
        }
    }

    pub fn total_required_bytes(&self) -> u64 {
        self.slices.iter().map(|s| s.required_bytes).sum()
    }
}
