use crate::arbiter::Arbiter;
use crate::error::{Error, Result};
use crate::freezer::{
    freeze_cgroup, freeze_process_signal, send_cooperative_yield_signal, thaw_cgroup,
    thaw_process_signal,
};
use crate::lease::{LeaseId, LeaseState};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::{sleep, Instant};
use tracing::{info, warn};

pub const DEFAULT_PREEMPT_TIMEOUT: Duration = Duration::from_millis(250);

pub type PreemptError = Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PreemptTier {
    Tier1Cooperative,
    Tier2ForcedFreeze,
    Revoked,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PreemptRecord {
    pub lease_id: LeaseId,
    pub plane_id: String,
    pub client_pid: Option<u32>,
    pub client_unit: Option<String>,
    pub tier: PreemptTier,
    pub completed: bool,
    pub deadline_ms: u64,
}

/// Two-Tier Preemption Coordinator for heterogeneous compute leases.
/// Tier 1: Emits cooperative yield notification (SIGUSR1 / Preempting) with a 250ms deadline.
/// Tier 2: Falls back to kernel freezing (cgroup.freeze or SIGSTOP) on deadline expiration.
pub struct PreemptCoordinator {
    arbiter: Arc<Arbiter>,
    deadline_duration: Duration,
    records: Arc<Mutex<HashMap<LeaseId, PreemptRecord>>>,
}

impl PreemptCoordinator {
    pub fn new(arbiter: Arc<Arbiter>) -> Self {
        Self::with_deadline(arbiter, DEFAULT_PREEMPT_TIMEOUT)
    }

    pub fn with_deadline(arbiter: Arc<Arbiter>, deadline: Duration) -> Self {
        Self {
            arbiter,
            deadline_duration: deadline,
            records: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Preempt a lease using the two-tier mechanism:
    /// Tier 1: Cooperative yield notification with 250ms timeout.
    /// Tier 2: Kernel freeze fallback if timeout expires.
    pub async fn preempt_lease(&self, lease_id: LeaseId) -> Result<()> {
        let lease = self
            .arbiter
            .get_lease(lease_id)
            .await
            .ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;

        if !lease.is_active() {
            return Ok(());
        }

        info!(
            "Starting Tier-1 cooperative preemption for lease {} on plane {}",
            lease_id, lease.plane_id
        );

        // Tier 1: Signal cooperative yield
        self.arbiter.yield_lease(lease_id).await?;
        if let Some(pid) = lease.client_pid {
            let _ = send_cooperative_yield_signal(pid);
        }

        {
            let mut recs = self.records.lock().await;
            if recs.len() > 128 {
                recs.retain(|_, r| !r.completed);
            }
            recs.insert(
                lease_id,
                PreemptRecord {
                    lease_id,
                    plane_id: lease.plane_id.clone(),
                    client_pid: lease.client_pid,
                    client_unit: lease.client_unit.clone(),
                    tier: PreemptTier::Tier1Cooperative,
                    completed: false,
                    deadline_ms: self.deadline_duration.as_millis() as u64,
                },
            );
        }

        // Wait for cooperative yield up to deadline
        let deadline = Instant::now() + self.deadline_duration;
        let poll_interval = Duration::from_millis(25);
        let mut cooperatively_yielded = false;

        while Instant::now() < deadline {
            sleep(poll_interval).await;
            if let Some(curr) = self.arbiter.get_lease(lease_id).await {
                if !curr.is_active()
                    || curr.state == LeaseState::Preempted
                    || curr.state == LeaseState::Expired
                    || curr.state == LeaseState::Revoked
                {
                    cooperatively_yielded = true;
                    break;
                }
            } else {
                cooperatively_yielded = true;
                break;
            }
        }

        if cooperatively_yielded {
            info!("Lease {} cooperatively yielded within deadline", lease_id);
            let mut recs = self.records.lock().await;
            if let Some(rec) = recs.get_mut(&lease_id) {
                rec.completed = true;
            }
            return Ok(());
        }

        // Tier 2: Deadline expired without cooperative yield — force kernel freeze
        warn!(
            "Tier-1 deadline expired for lease {}. Escalating to Tier-2 kernel freeze",
            lease_id
        );

        let mut frozen = false;
        if let Some(ref unit) = lease.client_unit {
            if freeze_cgroup(unit).is_ok() {
                frozen = true;
                info!("Frozen cgroup for unit {} under lease {}", unit, lease_id);
            }
        }

        if let Some(pid) = lease.client_pid {
            if freeze_process_signal(pid).is_ok() {
                frozen = true;
                info!("Sent SIGSTOP to PID {} under lease {}", pid, lease_id);
            }
        }

        self.arbiter.freeze_lease(lease_id).await?;

        {
            let mut recs = self.records.lock().await;
            if let Some(rec) = recs.get_mut(&lease_id) {
                rec.tier = PreemptTier::Tier2ForcedFreeze;
                rec.completed = frozen;
            }
        }

        Ok(())
    }

    /// Thaw a previously frozen lease.
    pub async fn thaw_lease(&self, lease_id: LeaseId) -> Result<()> {
        let lease = self
            .arbiter
            .get_lease(lease_id)
            .await
            .ok_or_else(|| Error::LeaseNotFound(lease_id.to_string()))?;

        if let Some(ref unit) = lease.client_unit {
            let _ = thaw_cgroup(unit);
        }
        if let Some(pid) = lease.client_pid {
            let _ = thaw_process_signal(pid);
        }

        self.arbiter.thaw_lease(lease_id).await
    }

    /// Forcibly revoke a lease and return its resources to the arbiter.
    pub async fn revoke_lease(&self, lease_id: LeaseId) -> Result<()> {
        self.arbiter.revoke_lease(lease_id).await?;
        let mut recs = self.records.lock().await;
        if let Some(rec) = recs.get_mut(&lease_id) {
            rec.tier = PreemptTier::Revoked;
            rec.completed = true;
        }
        Ok(())
    }

    pub async fn get_record(&self, lease_id: LeaseId) -> Option<PreemptRecord> {
        self.records.lock().await.get(&lease_id).cloned()
    }

    pub async fn list_records(&self) -> Vec<PreemptRecord> {
        self.records.lock().await.values().cloned().collect()
    }
}
