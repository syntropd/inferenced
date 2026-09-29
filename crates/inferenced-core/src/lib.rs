//! inferenced-core: Core hardware discovery, compute plane topology,
//! and resource scheduling primitives for systemd-inferenced.

pub mod error;
pub mod fd_lease;
pub mod lease;
pub mod model;
pub mod netlink;
pub mod peer;
pub mod psi;
pub mod reclaim;
pub mod schedule;
pub mod topology;

#[cfg(test)]
mod tests;

// Regrouped pages keep their crate-root paths, so callers are untouched.
pub use reclaim::{freezer, madvise, paging};
pub use schedule::{arbiter, gang_scheduler, preempt};

pub use arbiter::{Arbiter, ArbiterState, LeaseError, LeaseGrant, LeaseRequest};
pub use error::{Error, Result};
pub use fd_lease::{create_sealed_memfd, recv_fd_from_unix, send_fd_over_unix, FdLease};
pub use freezer::{
    freeze_cgroup, freeze_process_signal, is_cgroup_frozen, send_cooperative_yield_signal,
    signal_process, thaw_cgroup, thaw_process_signal, FreezeState,
};
pub use lease::{
    CompositeLease, CompositeLeaseRequest, ComputeLease, GangPolicy, LeaseId, LeasePriority,
    LeaseState, PlaneRole, PlaneSliceAllocation, SliceRequirement,
};
pub use madvise::{
    advise_dontneed, advise_hugepage, advise_random, advise_sequential, advise_willneed,
};
pub use model::{ModelDescriptor, ModelPlacementState, ModelRegistry};
pub use netlink::{open_uevent_socket, Uevent};
pub use paging::{MemfdPaging, PagingError, ZswapMetrics};
pub use peer::PeerInfo;
pub use preempt::{
    PreemptCoordinator, PreemptError, PreemptRecord, PreemptTier, DEFAULT_PREEMPT_TIMEOUT,
};
pub use psi::{PressureLevel, PressureMetrics};
pub use topology::{ComputePlane, ComputePlaneKind, HardwareTopology};

