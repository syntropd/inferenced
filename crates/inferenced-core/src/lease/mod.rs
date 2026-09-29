//! Lease primitives for single and composite gang compute slices.

pub mod composite;
pub mod request;
pub mod single;

pub use composite::{CompositeLease, GangPolicy, PlaneRole, PlaneSliceAllocation};
pub use request::{CompositeLeaseRequest, LeaseRequest, SliceRequirement};
pub use single::{ComputeLease, LeaseId, LeasePriority, LeaseState};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lease_defaults_and_exports() {
        let id = LeaseId::default();
        assert!(!id.to_string().is_empty());
        assert_eq!(GangPolicy::default(), GangPolicy::AllOrNothing);
    }
}
