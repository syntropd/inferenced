use super::composite_leases;
use super::leases;
use super::models;
use super::protocol::VarlinkReply;
use inferenced_core::{arbiter::Arbiter, lease::LeaseId, psi::PressureMetrics};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::unix::OwnedWriteHalf;

pub async fn handle_method(
    method: &str,
    params: Option<&Value>,
    arbiter: &Arc<Arbiter>,
    writer: &mut Option<OwnedWriteHalf>,
    active_leases: &mut Vec<LeaseId>,
    peer_info: Option<&inferenced_core::PeerInfo>,
) -> Option<VarlinkReply> {
    let sub = method.strip_prefix("io.syntrop.Inference1.")?;

    match sub {
        "GetStatus" | "GetInfo" => Some(handle_get_status(arbiter).await),
        "ListPlanes" | "ListDevices" => Some(handle_list_planes(arbiter).await),
        "GetTopology" => Some(handle_get_topology(arbiter).await),
        "GetPressure" => Some(handle_get_pressure()),
        "GetDrmWatermark" => Some(handle_get_drm_watermark(params)),
        "AcquireLease" => {
            Some(leases::handle_acquire_lease(params, arbiter, active_leases, peer_info).await)
        }
        "AcquireCompositeLease" => {
            Some(composite_leases::handle_acquire_composite_lease(params, arbiter, active_leases, peer_info).await)
        }
        "ReleaseLease" => {
            Some(leases::handle_release_lease(params, arbiter, active_leases).await)
        }
        "ReleaseCompositeLease" => {
            Some(composite_leases::handle_release_composite_lease(params, arbiter, active_leases).await)
        }
        "ResizeLease" => Some(handle_resize_lease(params, arbiter, active_leases).await),
        "Yield" => Some(leases::handle_yield(params, arbiter).await),
        "Freeze" | "FreezeLease" => Some(leases::handle_freeze(params, arbiter).await),
        "Thaw" | "ThawLease" => Some(leases::handle_thaw(params, arbiter).await),
        "ListLeases" => Some(leases::handle_list_leases(arbiter).await),
        "ListCompositeLeases" => Some(composite_leases::handle_list_composite_leases(arbiter).await),
        "ListModels" => Some(models::handle_list_models(arbiter).await),
        "RegisterModel" => {
            Some(models::handle_register_model(params, arbiter).await)
        }
        "EvictModel" => Some(models::handle_evict_model(params, arbiter).await),
        "PinModel" => Some(models::handle_pin_model(params, arbiter).await),
        "StreamInference" => {
            handle_stream_inference(params, arbiter, writer).await;
            None
        }
        _ => None,
    }
}

async fn handle_get_status(arbiter: &Arc<Arbiter>) -> VarlinkReply {
    let topo = arbiter.get_topology().await;
    let leases = arbiter.list_leases().await;
    let psi = PressureMetrics::read_current();
    VarlinkReply::ok(json!({
        "status": "active",
        "version": "0.1.0",
        "daemon": "systemd-inferenced",
        "planes_count": topo.planes.len(),
        "leases_count": leases.len(),
        "pressure": format!("{:?}", psi.level),
    }))
}

async fn handle_list_planes(arbiter: &Arc<Arbiter>) -> VarlinkReply {
    let topo = arbiter.get_topology().await;
    let planes: Vec<Value> = topo
        .planes
        .into_iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "kind": format!("{:?}", p.kind),
                "total_memory": p.total_memory_bytes,
                "available_memory": p.available_memory_bytes,
                "is_triage_reserved": p.is_triage_reserved,
                "features": p.hardware_features,
                "p2p_links": p.p2p_links,
                "kernel_used_memory": p.kernel_used_memory,
                "device_path": p.device_path,
            })
        })
        .collect();
    VarlinkReply::ok(json!({ "planes": planes }))
}

async fn handle_get_topology(arbiter: &Arc<Arbiter>) -> VarlinkReply {
    let topo = arbiter.get_topology().await;
    let planes: Vec<Value> = topo
        .planes
        .into_iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "kind": format!("{:?}", p.kind),
                "total_memory": p.total_memory_bytes,
                "available_memory": p.available_memory_bytes,
                "is_triage_reserved": p.is_triage_reserved,
                "features": p.hardware_features,
                "p2p_links": p.p2p_links,
                "kernel_used_memory": p.kernel_used_memory,
                "device_path": p.device_path,
            })
        })
        .collect();

    VarlinkReply::ok(json!({
        "planes": planes,
        "total_ram": topo.total_system_ram_bytes,
        "available_ram": topo.available_system_ram_bytes,
        "cpu_cores": topo.cpu_cores_total,
    }))
}

fn handle_get_pressure() -> VarlinkReply {
    let psi = PressureMetrics::read_current();
    VarlinkReply::ok(json!({
        "level": format!("{:?}", psi.level),
        "cpu_some": psi.cpu_some_avg10,
        "memory_some": psi.memory_some_avg10,
        "io_some": psi.io_some_avg10,
    }))
}

async fn handle_stream_inference(
    params: Option<&Value>,
    _arbiter: &Arc<Arbiter>,
    writer: &mut Option<OwnedWriteHalf>,
) {
    let w = match writer {
        Some(w) => w,
        None => return,
    };
    let prompt = params
        .and_then(|p| p.get("prompt"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tokens = [
        "Inference: ",
        "Analyzing prompt '",
        prompt,
        "'. Execution dispatched via systemd-inferenced.",
    ];

    for (i, token) in tokens.iter().enumerate() {
        let continues = i < tokens.len() - 1;
        let reply = VarlinkReply::streaming(json!({ "chunk": token }), continues);
        let _ = w.write_all(&reply.to_bytes()).await;
        let _ = w.flush().await;
    }
}

fn handle_get_drm_watermark(params: Option<&Value>) -> VarlinkReply {
    let dev_filter = params.and_then(|p| p.get("device")).and_then(|v| v.as_str());
    let watermarks = inferenced_core::reclaim::drm_watermark::sample_drm_watermarks();
    let list: Vec<Value> = watermarks
        .into_iter()
        .filter(|wm| {
            if let Some(target) = dev_filter {
                wm.render_device == target || wm.render_device.ends_with(target)
            } else {
                true
            }
        })
        .map(|wm| {
            json!({
                "device": wm.render_device,
                "vendor": wm.vendor,
                "vram_used": wm.vram_used_bytes,
                "vram_total": wm.vram_total_bytes,
                "used_percentage": wm.used_ratio * 100.0,
                "fallback_psi": wm.is_fallback_psi,
            })
        })
        .collect();
    VarlinkReply::ok(json!({ "watermarks": list }))
}

#[allow(clippy::ptr_arg)]
async fn handle_resize_lease(
    params: Option<&Value>,
    arbiter: &Arc<Arbiter>,
    active_leases: &mut Vec<LeaseId>,
) -> VarlinkReply {
    let params = match params {
        Some(p) => p,
        None => return VarlinkReply::error("org.varlink.service.InvalidParameter", json!({"parameter": "parameters"})),
    };
    let id_str = params.get("lease_id").and_then(|v| v.as_str()).unwrap_or("");
    let lease_id = match uuid::Uuid::parse_str(id_str) {
        Ok(u) => LeaseId(u),
        Err(_) => return VarlinkReply::error("org.varlink.service.InvalidParameter", json!({"parameter": "lease_id"})),
    };
    let mem_bytes = match params.get("memory_bytes").and_then(|v| v.as_u64()) {
        Some(m) if m > 0 => m,
        _ => return VarlinkReply::error("org.varlink.service.InvalidParameter", json!({"parameter": "memory_bytes"})),
    };

    if !active_leases.contains(&lease_id) {
        return VarlinkReply::error(
            "io.syntrop.Inference1.LeaseNotFound",
            json!({"lease_id": lease_id.to_string()}),
        );
    }

    match arbiter.resize_lease(lease_id, mem_bytes).await {
        Ok(lease) => VarlinkReply::ok(json!({
            "lease_id": lease.id.to_string(),
            "plane_id": lease.plane_id,
            "allocated_memory": lease.allocated_memory_bytes,
        })),
        Err(inferenced_core::error::Error::LeaseNotFound(_)) => {
            VarlinkReply::error("io.syntrop.Inference1.LeaseNotFound", json!({"lease_id": lease_id.to_string()}))
        }
        Err(inferenced_core::error::Error::ResourceExhaustion { plane, requested_bytes, available_bytes }) => {
            VarlinkReply::error("io.syntrop.Inference1.ResourceExhaustion", json!({
                "plane": plane,
                "requested": requested_bytes,
                "available": available_bytes,
            }))
        }
        Err(e) => VarlinkReply::error("org.varlink.service.InvalidParameter", json!({"error": e.to_string()})),
    }
}

