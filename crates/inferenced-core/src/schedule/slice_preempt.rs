//! systemd cgroup slice priority binding wiring PreemptCoordinator to user.slice.

use crate::arbiter::Arbiter;
use crate::error::Result;
use crate::lease::{LeaseId, LeasePriority};
use crate::schedule::preempt::PreemptCoordinator;
use crate::topology::ComputePlaneKind;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{error, info, warn};

pub const DEFAULT_USER_SLICE_PROCS: &str = "/sys/fs/cgroup/user.slice/cgroup.procs";
pub const DEFAULT_PROC_ROOT: &str = "/proc";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SlicePreemptStatus {
    pub interactive_procs_count: usize,
    pub interactive_pids: Vec<u32>,
    pub preempted_leases: Vec<LeaseId>,
}

/// Binds user session slice activity to GPU compute lease preemption.
#[derive(Clone)]
pub struct SlicePreemptCoordinator {
    preempt: Arc<PreemptCoordinator>,
    arbiter: Arc<Arbiter>,
    user_slice_procs: PathBuf,
    proc_root: PathBuf,
}

impl SlicePreemptCoordinator {
    pub fn new(preempt: Arc<PreemptCoordinator>, arbiter: Arc<Arbiter>) -> Self {
        Self::with_paths(preempt, arbiter, PathBuf::from(DEFAULT_USER_SLICE_PROCS), PathBuf::from(DEFAULT_PROC_ROOT))
    }

    pub fn with_paths(preempt: Arc<PreemptCoordinator>, arbiter: Arc<Arbiter>, user_slice_procs: PathBuf, proc_root: PathBuf) -> Self {
        Self { preempt, arbiter, user_slice_procs, proc_root }
    }

    fn collect_pids_from_cgroup_dir(dir: &Path, pids: &mut Vec<u32>, depth: usize) {
        if depth > 5 { return; }
        if let Ok(c) = fs::read_to_string(dir.join("cgroup.procs")) {
            pids.extend(c.lines().filter_map(|l| l.trim().parse::<u32>().ok()));
        }
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                    Self::collect_pids_from_cgroup_dir(&entry.path(), pids, depth + 1);
                }
            }
        }
    }

    /// Read PIDs belonging to user.slice and any delegated child cgroups
    pub fn list_user_slice_pids(&self) -> Vec<u32> {
        let mut pids = Vec::new();
        if let Ok(c) = fs::read_to_string(&self.user_slice_procs) {
            pids.extend(c.lines().filter_map(|l| l.trim().parse::<u32>().ok()));
        }
        let base_dir = if self.user_slice_procs.is_dir() {
            Some(self.user_slice_procs.as_path())
        } else {
            self.user_slice_procs.parent().filter(|p| p.is_dir())
        };
        if let Some(dir) = base_dir {
            Self::collect_pids_from_cgroup_dir(dir, &mut pids, 0);
        }
        pids.sort_unstable();
        pids.dedup();
        pids
    }

    /// Detect if a process has opened any DRM render node (/dev/dri/renderD*)
    pub fn process_has_drm_fd(&self, pid: u32) -> bool {
        let fd_dir = self.proc_root.join(pid.to_string()).join("fd");
        let entries = match fs::read_dir(fd_dir) {
            Ok(e) => e,
            Err(_) => return false,
        };
        for entry in entries.flatten() {
            if let Ok(target) = fs::read_link(entry.path()) {
                let s = target.to_string_lossy();
                if s.contains("/dev/dri/renderD")
                    || s.contains("renderD")
                    || s.contains("/dev/dri/card")
                    || s.contains("/dev/kfd")
                {
                    return true;
                }
            }
        }
        false
    }

    /// Scan all user.slice processes and identify those actively holding DRM render nodes
    pub fn scan_interactive_drm_consumers(&self) -> Vec<u32> {
        self.list_user_slice_pids()
            .into_iter()
            .filter(|&pid| self.process_has_drm_fd(pid))
            .collect()
    }

    /// Evaluate system.slice leases and trigger Tier-1/Tier-2 preemption if user.slice needs GPU
    pub async fn evaluate_and_preempt(&self) -> Result<SlicePreemptStatus> {
        let coordinator = self.clone();
        let interactive_pids = tokio::task::spawn_blocking(move || {
            coordinator.scan_interactive_drm_consumers()
        })
        .await
        .map_err(|e| crate::error::Error::PreemptionFailed(format!("spawn_blocking error: {e}")))?;

        let psi = crate::psi::PressureMetrics::read_current();
        let cpu_contention = psi.runqueue_latency_us > 30_000
            || psi.cpu_some_avg10 > 50.0
            || psi.level != crate::psi::PressureLevel::Normal;

        let mut preempted = Vec::new();
        if !interactive_pids.is_empty() || cpu_contention {
            if cpu_contention {
                warn!(
                    "CPU runqueue contention spike ({} us); triggering cooperative preemption",
                    psi.runqueue_latency_us
                );
            }
            if !interactive_pids.is_empty() {
                info!("Interactive user session PIDs {:?} hold DRM render nodes", interactive_pids);
            }
            let topo = self.arbiter.get_topology().await;
            let leases = self.arbiter.list_leases().await;
            let mut candidates = Vec::new();
            for lease in leases {
                let is_gpu = topo.planes.iter()
                    .find(|p| p.id == lease.plane_id)
                    .map(|p| matches!(p.kind, ComputePlaneKind::DiscreteGpu | ComputePlaneKind::IntegratedUma))
                    .unwrap_or(true);
                let is_bg = lease.client_unit.as_deref().is_some_and(|u| {
                    u.contains("system.slice") || u.starts_with("system-")
                }) || lease.priority == LeasePriority::Batch;

                if is_gpu && is_bg && lease.is_active() {
                    candidates.push(lease.id);
                }
            }

            let tasks = candidates.into_iter().map(|id| {
                let p = self.preempt.clone();
                async move {
                    warn!("Preempting background GPU lease {} for interactive session / contention", id);
                    (id, p.preempt_lease(id).await)
                }
            });
            for (id, res) in futures::future::join_all(tasks).await {
                match res {
                    Ok(_) => preempted.push(id),
                    Err(e) => error!("Failed to preempt background GPU lease {}: {}", id, e),
                }
            }
        }

        Ok(SlicePreemptStatus {
            interactive_procs_count: interactive_pids.len(),
            interactive_pids,
            preempted_leases: preempted,
        })
    }
}
