//! Single-device compute lease representation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LeaseId(pub Uuid);

impl Default for LeaseId {
    fn default() -> Self {
        Self(Uuid::new_v4())
    }
}

impl std::fmt::Display for LeaseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LeasePriority {
    /// Lowest priority: bulk image generation, dataset embedding, offline indexing
    Batch = 0,
    /// Medium priority: interactive CLI, user chat completions, agent tool-calling
    Interactive = 10,
    /// Highest priority: systemd-sentry crash triage and kernel panic diagnosis
    EmergencyTriage = 100,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaseState {
    Active,
    Preempting,
    Frozen,
    Preempted,
    Revoked,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputeLease {
    pub id: LeaseId,
    pub client_pid: Option<u32>,
    pub client_unit: Option<String>,
    pub plane_id: String,
    pub allocated_memory_bytes: u64,
    pub priority: LeasePriority,
    pub state: LeaseState,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl ComputeLease {
    pub fn new(
        plane_id: String,
        allocated_memory_bytes: u64,
        priority: LeasePriority,
        client_unit: Option<String>,
        client_pid: Option<u32>,
    ) -> Self {
        Self {
            id: LeaseId::default(),
            client_pid,
            client_unit,
            plane_id,
            allocated_memory_bytes,
            priority,
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
}
