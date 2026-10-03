use super::protocol::VarlinkReply;
use inferenced_core::{
    arbiter::Arbiter,
    lease::{LeaseId, LeasePriority},
};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub async fn handle_acquire_lease(
    params: Option<&Value>,
    arbiter: &Arc<Arbiter>,
    active_leases: &mut Vec<LeaseId>,
    peer_info: Option<&inferenced_core::PeerInfo>,
) -> VarlinkReply {
    let params = match params {
        Some(p) => p,
        None => {
            return VarlinkReply::error(
                "org.varlink.service.InvalidParameter",
                json!({"parameter": "parameters"}),
            )
        }
    };

    let prio_str = params
        .get("priority")
        .and_then(|v| v.as_str())
        .unwrap_or("Interactive");
    let priority = match prio_str {
        "Batch" => LeasePriority::Batch,
        "EmergencyTriage" => LeasePriority::EmergencyTriage,
        _ => LeasePriority::Interactive,
    };

    let mem_bytes = params
        .get("memory_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(1024 * 1024 * 1024);
    let plane = params
        .get("plane")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    let unit = params
        .get("unit")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    let pid = params.get("pid").and_then(|v| v.as_u64()).map(|p| p as u32);
    let workload = params
        .get("workload")
        .or_else(|| params.get("workload_kind"))
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<inferenced_core::topology::WorkloadKind>().ok());

    // Secure peer attribution: Use verified peer credentials and cgroup slice
    let (verified_unit, verified_pid, priority) = if let Some(peer) = peer_info {
        let p = if peer.is_batch {
            LeasePriority::Batch
        } else {
            priority
        };
        (Some(peer.slice.clone()), if peer.pid > 0 { Some(peer.pid) } else { pid }, p)
    } else {
        (unit, pid, priority)
    };

    match arbiter
        .acquire_lease_with_workload(priority, mem_bytes, plane, verified_unit, verified_pid, workload)
        .await
    {
        Ok(lease) => {
            active_leases.push(lease.id);
            VarlinkReply::ok(json!({
                "lease_id": lease.id.to_string(),
                "plane_id": lease.plane_id,
                "allocated_memory": lease.allocated_memory_bytes,
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

pub async fn handle_release_lease(
    params: Option<&Value>,
    arbiter: &Arc<Arbiter>,
    active_leases: &mut Vec<LeaseId>,
) -> VarlinkReply {
    let lease_id = match parse_lease_id(params) {
        Ok(id) => id,
        Err(e) => return e,
    };

    match arbiter.release_lease(lease_id).await {
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

pub async fn handle_yield(params: Option<&Value>, arbiter: &Arc<Arbiter>) -> VarlinkReply {
    let lease_id = match parse_lease_id(params) {
        Ok(id) => id,
        Err(e) => return e,
    };
    match arbiter.yield_lease(lease_id).await {
        Ok(_) => VarlinkReply::ok(json!({})),
        Err(e) => VarlinkReply::error("io.syntrop.Inference1.LeaseNotFound", json!({"error": e.to_string()})),
    }
}

pub async fn handle_freeze(params: Option<&Value>, arbiter: &Arc<Arbiter>) -> VarlinkReply {
    let lease_id = match parse_lease_id(params) {
        Ok(id) => id,
        Err(e) => return e,
    };
    match arbiter.freeze_lease(lease_id).await {
        Ok(_) => VarlinkReply::ok(json!({})),
        Err(e) => VarlinkReply::error("io.syntrop.Inference1.LeaseNotFound", json!({"error": e.to_string()})),
    }
}

pub async fn handle_thaw(params: Option<&Value>, arbiter: &Arc<Arbiter>) -> VarlinkReply {
    let lease_id = match parse_lease_id(params) {
        Ok(id) => id,
        Err(e) => return e,
    };
    match arbiter.thaw_lease(lease_id).await {
        Ok(_) => VarlinkReply::ok(json!({})),
        Err(e) => VarlinkReply::error("io.syntrop.Inference1.ResourceExhaustion", json!({"error": e.to_string()})),
    }
}

fn parse_lease_id(params: Option<&Value>) -> Result<LeaseId, VarlinkReply> {
    let id_str = params
        .and_then(|p| p.get("lease_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    Uuid::parse_str(id_str)
        .map(LeaseId)
        .map_err(|_| VarlinkReply::error("org.varlink.service.InvalidParameter", json!({"parameter": "lease_id"})))
}

pub async fn handle_list_leases(arbiter: &Arc<Arbiter>) -> VarlinkReply {
    let leases = arbiter.list_leases().await;
    let list: Vec<Value> = leases
        .into_iter()
        .map(|l| {
            json!({
                "id": l.id.to_string(),
                "plane_id": l.plane_id,
                "allocated_memory": l.allocated_memory_bytes,
                "priority": format!("{:?}", l.priority),
                "state": format!("{:?}", l.state),
                "client_unit": l.client_unit,
                "client_pid": l.client_pid,
            })
        })
        .collect();
    VarlinkReply::ok(json!({ "leases": list }))
}
