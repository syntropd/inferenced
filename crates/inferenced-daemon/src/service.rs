//! Background daemon service loops for watchdog, inhibitor, and slice preemption.

use inferenced_core::{
    arbiter::Arbiter,
    preempt::PreemptCoordinator,
    schedule::slice_preempt::SlicePreemptCoordinator,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tracing::{error, info};

pub const DEFAULT_PREEMPT_INTERVAL: Duration = Duration::from_millis(500);

/// Spawns the periodic evaluation loop for SlicePreemptCoordinator.
pub fn spawn_slice_preempt_service(
    coordinator: Arc<SlicePreemptCoordinator>,
    interval: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(interval);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            match coordinator.evaluate_and_preempt().await {
                Ok(status) if !status.preempted_leases.is_empty() => {
                    info!(
                        preempted_count = status.preempted_leases.len(),
                        interactive_pids = ?status.interactive_pids,
                        "Slice preemption reclaimed GPU leases for interactive user session"
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    error!("SlicePreemptCoordinator evaluation failed: {}", e);
                }
            }
        }
    })
}

/// Helper initializing SlicePreemptCoordinator and spawning its periodic evaluation service loop.
pub fn schedule_slice_preemption(
    preempt: Arc<PreemptCoordinator>,
    arbiter: Arc<Arbiter>,
    interval: Duration,
) -> (Arc<SlicePreemptCoordinator>, JoinHandle<()>) {
    let coordinator = Arc::new(SlicePreemptCoordinator::new(preempt, arbiter));
    let handle = spawn_slice_preempt_service(coordinator.clone(), interval);
    (coordinator, handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use inferenced_core::topology::{ComputePlane, ComputePlaneKind, HardwareTopology};

    fn make_test_topo() -> HardwareTopology {
        let mut topo = HardwareTopology::default();
        topo.planes.push(
            ComputePlane::builder("plane-gpu-0")
                .name("Test GPU")
                .kind(ComputePlaneKind::DiscreteGpu)
                .no_device_path()
                .total_memory(8 * 1024 * 1024 * 1024)
                .build(),
        );
        topo
    }

    #[tokio::test]
    async fn test_service_schedule_slice_preemption() {
        let arbiter = Arc::new(Arbiter::new(make_test_topo()));
        let preempt = Arc::new(PreemptCoordinator::new(arbiter.clone()));
        let (_coord, handle) = schedule_slice_preemption(
            preempt,
            arbiter,
            Duration::from_millis(10),
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        handle.abort();
    }
}
