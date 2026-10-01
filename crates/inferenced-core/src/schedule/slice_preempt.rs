//! systemd cgroup slice priority binding wiring PreemptCoordinator to user.slice.

use crate::arbiter::Arbiter;
use crate::error::Result;
use crate::lease::{LeaseId, LeasePriority};
use crate::schedule::preempt::PreemptCoordinator;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

pub const DEFAULT_USER_SLICE_PROCS: &str = "/sys/fs/cgroup/user.slice/cgroup.procs";
pub const DEFAULT_PROC_ROOT: &str = "/proc";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SlicePreemptStatus {
    pub interactive_procs_count: usize,
    pub interactive_pids: Vec<u32>,
    pub preempted_leases: Vec<LeaseId>,
}

/// Binds user session slice activity to GPU compute lease preemption.
pub struct SlicePreemptCoordinator {
    preempt: Arc<PreemptCoordinator>,
    arbiter: Arc<Arbiter>,
    user_slice_procs: PathBuf,
    proc_root: PathBuf,
}

impl SlicePreemptCoordinator {
    pub fn new(preempt: Arc<PreemptCoordinator>, arbiter: Arc<Arbiter>) -> Self {
        Self::with_paths(
            preempt,
            arbiter,
            PathBuf::from(DEFAULT_USER_SLICE_PROCS),
            PathBuf::from(DEFAULT_PROC_ROOT),
        )
    }

    pub fn with_paths(
        preempt: Arc<PreemptCoordinator>,
        arbiter: Arc<Arbiter>,
        user_slice_procs: PathBuf,
        proc_root: PathBuf,
    ) -> Self {
        Self {
            preempt,
            arbiter,
            user_slice_procs,
            proc_root,
        }
    }

    /// Read PIDs belonging to user.slice
    pub fn list_user_slice_pids(&self) -> Vec<u32> {
        let content = match fs::read_to_string(&self.user_slice_procs) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        content
            .lines()
            .filter_map(|l| l.trim().parse::<u32>().ok())
            .collect()
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
                if s.contains("/dev/dri/renderD") || s.contains("renderD") {
                    return true;
                }
            }
        }
        false
    }

    /// Scan all user.slice processes and identify those actively holding DRM render nodes
    pub fn scan_interactive_drm_consumers(&self) -> Vec<u32> {
        let pids = self.list_user_slice_pids();
        pids.into_iter()
            .filter(|&pid| self.process_has_drm_fd(pid))
            .collect()
    }

    /// Evaluate system.slice leases and trigger Tier-1/Tier-2 preemption if user.slice needs GPU
    pub async fn evaluate_and_preempt(&self) -> Result<SlicePreemptStatus> {
        let interactive_pids = self.scan_interactive_drm_consumers();
        let mut preempted = Vec::new();

        if !interactive_pids.is_empty() {
            info!(
                "Interactive user session processes {:?} opened DRM nodes; evaluating background leases",
                interactive_pids
            );

            let leases = self.arbiter.list_leases().await;
            for lease in leases {
                let is_background = lease.client_unit.as_deref().is_some_and(|u| {
                    u.contains("system.slice") || u.starts_with("system-")
                }) || lease.priority == LeasePriority::Batch;

                if is_background && lease.is_active() {
                    warn!(
                        "Preempting background lease {} ({:?}) in favor of interactive user session",
                        lease.id, lease.client_unit
                    );
                    if self.preempt.preempt_lease(lease.id).await.is_ok() {
                        preempted.push(lease.id);
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lease::LeaseState;
    use crate::topology::{ComputePlane, ComputePlaneKind, HardwareTopology};
    use tempfile::tempdir;

    fn make_test_topo() -> HardwareTopology {
        let mut topo = HardwareTopology::default();
        topo.planes.push(ComputePlane {
            id: "plane-gpu-0".into(),
            name: "Test GPU".into(),
            kind: ComputePlaneKind::DiscreteGpu,
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
        topo
    }

    #[tokio::test]
    async fn test_slice_preempt_background_leases() {
        let dir = tempdir().unwrap();
        let user_slice_procs = dir.path().join("cgroup.procs");
        let proc_root = dir.path().join("proc");

        // Write user.slice PID 4242
        fs::write(&user_slice_procs, "4242\n").unwrap();

        // Create /proc/4242/fd/3 pointing to /dev/dri/renderD128
        let fd_dir = proc_root.join("4242/fd");
        fs::create_dir_all(&fd_dir).unwrap();
        let target_dri = dir.path().join("renderD128");
        fs::write(&target_dri, "").unwrap();
        std::os::unix::fs::symlink(&target_dri, fd_dir.join("3")).unwrap();

        let arbiter = Arc::new(Arbiter::new(make_test_topo()));
        let preempt = Arc::new(PreemptCoordinator::new(arbiter.clone()));

        // Acquire background system.slice lease
        let bg_lease = arbiter
            .acquire_lease(
                LeasePriority::Batch,
                1024 * 1024 * 1024,
                Some("plane-gpu-0".into()),
                Some("system.slice/background-job.service".into()),
                Some(9999),
            )
            .await
            .unwrap();

        let coordinator = SlicePreemptCoordinator::with_paths(
            preempt.clone(),
            arbiter.clone(),
            user_slice_procs,
            proc_root,
        );

        let status = coordinator.evaluate_and_preempt().await.unwrap();
        assert_eq!(status.interactive_pids, vec![4242]);
        assert_eq!(status.preempted_leases, vec![bg_lease.id]);

        let updated_lease = arbiter.get_lease(bg_lease.id).await.unwrap();
        assert_eq!(updated_lease.state, LeaseState::Frozen);
    }

    #[tokio::test]
    async fn test_slice_preempt_no_interactive_consumers() {
        let dir = tempdir().unwrap();
        let user_slice_procs = dir.path().join("cgroup.procs");
        let proc_root = dir.path().join("proc");

        // Write user.slice PID 1000 with no DRM fds
        fs::write(&user_slice_procs, "1000\n").unwrap();
        let fd_dir = proc_root.join("1000/fd");
        fs::create_dir_all(&fd_dir).unwrap();
        let regular_file = dir.path().join("regular.txt");
        fs::write(&regular_file, "data").unwrap();
        std::os::unix::fs::symlink(&regular_file, fd_dir.join("0")).unwrap();

        let arbiter = Arc::new(Arbiter::new(make_test_topo()));
        let preempt = Arc::new(PreemptCoordinator::new(arbiter.clone()));

        let bg_lease = arbiter
            .acquire_lease(
                LeasePriority::Batch,
                1024 * 1024 * 1024,
                Some("plane-gpu-0".into()),
                Some("system.slice/batch.service".into()),
                Some(8888),
            )
            .await
            .unwrap();

        let coordinator = SlicePreemptCoordinator::with_paths(
            preempt.clone(),
            arbiter.clone(),
            user_slice_procs,
            proc_root,
        );

        let status = coordinator.evaluate_and_preempt().await.unwrap();
        assert!(status.interactive_pids.is_empty());
        assert!(status.preempted_leases.is_empty());

        let lease = arbiter.get_lease(bg_lease.id).await.unwrap();
        assert!(lease.is_active());
    }
}
