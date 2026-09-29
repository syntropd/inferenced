//! Composite multi-accelerator gang lease definitions.

use super::single::{LeaseId, LeasePriority, LeaseState};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Execution role of a compute plane within a gang-scheduled allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlaneRole {
    Primary,
    Worker,
    Embedding,
    Backbone,
    Head,
    Rank(u32),
    Draft,
    Target,
}

impl std::fmt::Display for PlaneRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Primary => write!(f, "primary"),
            Self::Worker => write!(f, "worker"),
            Self::Embedding => write!(f, "embedding"),
            Self::Backbone => write!(f, "backbone"),
            Self::Head => write!(f, "head"),
            Self::Rank(r) => write!(f, "rank-{}", r),
            Self::Draft => write!(f, "draft"),
            Self::Target => write!(f, "target"),
        }
    }
}

/// Scheduling policy for gang multi-accelerator allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum GangPolicy {
    /// Atomic all-or-nothing: either all slices succeed or none are granted.
    #[default]
    AllOrNothing,
    /// Best effort: grant as many requested slices as possible.
    BestEffort,
    /// Strict PCIe/NUMA affinity requirement across all members of the gang.
    StrictAffinity,
}

/// A discrete plane slice allocated as part of a composite lease.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaneSliceAllocation {
    pub plane_id: String,
    pub role: PlaneRole,
    pub allocated_memory_bytes: u64,
    pub numa_node: Option<u32>,
    pub device_path: Option<PathBuf>,
}

/// A composite lease coordinating multiple plane slices as a single gang.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompositeLease {
    pub id: LeaseId,
    pub client_pid: Option<u32>,
    pub client_unit: Option<String>,
    pub slices: Vec<PlaneSliceAllocation>,
    pub priority: LeasePriority,
    pub policy: GangPolicy,
    pub state: LeaseState,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl CompositeLease {
    pub fn new(
        slices: Vec<PlaneSliceAllocation>,
        priority: LeasePriority,
        policy: GangPolicy,
        client_unit: Option<String>,
        client_pid: Option<u32>,
    ) -> Self {
        Self {
            id: LeaseId::default(),
            client_pid,
            client_unit,
            slices,
            priority,
            policy,
            state: LeaseState::Active,
            created_at: Utc::now(),
            expires_at: None,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            LeaseState::Active | LeaseState::Preempting | LeaseState::Frozen
        )
    }

    pub fn total_allocated_memory_bytes(&self) -> u64 {
        self.slices.iter().map(|s| s.allocated_memory_bytes).sum()
    }

    pub fn plane_ids(&self) -> Vec<&str> {
        self.slices.iter().map(|s| s.plane_id.as_str()).collect()
    }
}
