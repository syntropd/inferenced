//! Varlink dispatch for composite gang leases.

use super::protocol::VarlinkReply;
use inferenced_core::{
    arbiter::Arbiter,
    lease::{
        CompositeLeaseRequest, GangPolicy, LeaseId, LeasePriority, PlaneRole, SliceRequirement,
    },
};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub async fn handle_acquire_composite_lease(
    params: Option<&Value>,
    arbiter: &Arc<Arbiter>,
    active_leases: &mut Vec<LeaseId>,
    peer_info: Option<&inferenced_core::PeerInfo>,
) -> VarlinkReply {
    let params = match params {
        Some(p) => p,
        None => return VarlinkReply::error("org.varlink.service.InvalidParameter", json!({"parameter": "parameters"})),
    };

    let priority = match params.get("priority").and_then(|v| v.as_str()).unwrap_or("Interactive") {
        "Batch" => LeasePriority::Batch,
        "EmergencyTriage" => LeasePriority::EmergencyTriage,
        _ => LeasePriority::Interactive,
    };

    let policy = match params.get("policy").and_then(|v| v.as_str()).unwrap_or("AllOrNothing") {
        "BestEffort" => GangPolicy::BestEffort,
        "StrictAffinity" => GangPolicy::StrictAffinity,
        _ => GangPolicy::AllOrNothing,
    };

    let slices_raw = match params.get("slices").and_then(|v| v.as_array()) {
        Some(s) => s,
        None => return VarlinkReply::error("org.varlink.service.InvalidParameter", json!({"parameter": "slices"})),
    };

    let mut slices = Vec::new();
    for s in slices_raw {
        let role = match s.get("role").and_then(|v| v.as_str()).unwrap_or("Primary") {
            "Worker" => PlaneRole::Worker,
            "Embedding" => PlaneRole::Embedding,
            "Backbone" => PlaneRole::Backbone,
            "Head" => PlaneRole::Head,
            _ => PlaneRole::Primary,
        };
        let required_bytes = s.get("memory_bytes").and_then(|v| v.as_u64()).unwrap_or(1024 * 1024 * 1024);
        let preferred_plane = s.get("plane").and_then(|v| v.as_str()).map(ToString::to_string);
        slices.push(SliceRequirement { role, required_bytes, preferred_plane });
    }

    let unit = params.get("unit").and_then(|v| v.as_str()).map(ToString::to_string);
    let pid = params.get("pid").and_then(|v| v.as_u64()).map(|p| p as u32);

    let (verified_unit, verified_pid, priority) = if let Some(peer) = peer_info {
        let p = if peer.is_batch { LeasePriority::Batch } else { priority };
        (Some(peer.slice.clone()), if peer.pid > 0 { Some(peer.pid) } else { pid }, p)
    } else {
        (unit, pid, priority)
    };

    let req = CompositeLeaseRequest::new(slices, priority, policy, verified_unit, verified_pid);
    let workload = params
        .get("workload")
        .or_else(|| params.get("workload_kind"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<inferenced_core::topology::WorkloadKind>().ok());

    if let Some(w) = workload {
        let topo = arbiter.get_topology().await;
        if let Err(inferenced_core::Error::HardwareIncompatible {
            deficit,
            estimated_cpu_latency_secs,
            suggested_alternatives,
        }) = inferenced_core::schedule::check_workload_compatibility(&topo, w) {
            return VarlinkReply::error(
                "io.syntrop.Inference1.HardwareIncompatible",
                json!({
                    "deficit": deficit,
                    "estimated_cpu_latency_secs": estimated_cpu_latency_secs,
                    "suggested_alternatives": suggested_alternatives,
                }),
            );
        }
    }

    match arbiter.acquire_composite_lease(req).await {
        Ok(lease) => {
            active_leases.push(lease.id);
            let slices_json: Vec<Value> = lease.slices.iter().map(|s| {
                json!({
                    "plane_id": s.plane_id,
                    "role": s.role.to_string(),
                    "allocated_memory": s.allocated_memory_bytes,
                    "numa_node": s.numa_node,
                    "device_path": s.device_path,
                })
            }).collect();
            VarlinkReply::ok(json!({
                "lease_id": lease.id.to_string(),
                "slices": slices_json,
                "priority": format!("{:?}", lease.priority),
                "policy": format!("{:?}", lease.policy),
            }))
        }
        Err(inferenced_core::Error::HardwareIncompatible {
            deficit,
            estimated_cpu_latency_secs,
            suggested_alternatives,
        }) => VarlinkReply::error(
            "io.syntrop.Inference1.HardwareIncompatible",
            json!({
                "deficit": deficit,
                "estimated_cpu_latency_secs": estimated_cpu_latency_secs,
                "suggested_alternatives": suggested_alternatives,
            }),
        ),
        Err(e) => VarlinkReply::error(
            "io.syntrop.Inference1.ResourceExhaustion",
            json!({"error": e.to_string()}),
        ),
    }
}

pub async fn handle_release_composite_lease(
    params: Option<&Value>,
    arbiter: &Arc<Arbiter>,
    active_leases: &mut Vec<LeaseId>,
) -> VarlinkReply {
    let lease_id = match parse_lease_id(params) {
        Ok(id) => id,
        Err(e) => return e,
    };

    match arbiter.release_composite_lease(lease_id).await {
        Ok(_) => {
            active_leases.retain(|&id| id != lease_id);
            VarlinkReply::ok(json!({}))
        }
        Err(e) => VarlinkReply::error(
            "io.syntrop.Inference1.LeaseNotFound",
            json!({"error": e.to_string()}),
        ),
    }
}

pub async fn handle_list_composite_leases(arbiter: &Arc<Arbiter>) -> VarlinkReply {
    let leases = arbiter.list_composite_leases().await;
    let list: Vec<Value> = leases.into_iter().map(|l| {
        let slices: Vec<Value> = l.slices.iter().map(|s| {
            json!({
                "plane_id": s.plane_id,
                "role": s.role.to_string(),
                "allocated_memory": s.allocated_memory_bytes,
                "numa_node": s.numa_node,
                "device_path": s.device_path,
            })
        }).collect();
        json!({
            "lease_id": l.id.to_string(),
            "slices": slices,
            "priority": format!("{:?}", l.priority),
            "policy": format!("{:?}", l.policy),
            "state": format!("{:?}", l.state),
            "client_unit": l.client_unit,
            "client_pid": l.client_pid,
        })
    }).collect();
    VarlinkReply::ok(json!({ "composite_leases": list }))
}

fn parse_lease_id(params: Option<&Value>) -> std::result::Result<LeaseId, VarlinkReply> {
    let id_str = params
        .and_then(|p| p.get("lease_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            VarlinkReply::error(
                "org.varlink.service.InvalidParameter",
                json!({"parameter": "lease_id"}),
            )
        })?;

    Uuid::parse_str(id_str)
        .map(LeaseId)
        .map_err(|_| {
            VarlinkReply::error(
                "org.varlink.service.InvalidParameter",
                json!({"parameter": "lease_id", "reason": "malformed UUID"}),
            )
        })
}
