use super::protocol::VarlinkReply;
use inferenced_core::{
    arbiter::Arbiter,
    model::{ModelDescriptor, ModelPlacementState},
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub async fn handle_list_models(arbiter: &Arc<Arbiter>) -> VarlinkReply {
    let models = arbiter.list_models().await;
    let list: Vec<Value> = models
        .into_iter()
        .map(|m| {
            json!({
                "id": m.id,
                "format": m.format,
                "path": m.path.to_string_lossy(),
                "estimated_memory": m.estimated_memory_bytes,
                "placement": format!("{:?}", m.placement),
            })
        })
        .collect();
    VarlinkReply::ok(json!({ "models": list }))
}

pub async fn handle_register_model(params: Option<&Value>, arbiter: &Arc<Arbiter>) -> VarlinkReply {
    let p = match params {
        Some(v) => v,
        None => {
            return VarlinkReply::error(
                "org.varlink.service.InvalidParameter",
                json!({"parameter": "parameters"}),
            )
        }
    };
    let id = p.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let format = p
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("GGUF")
        .to_string();
    let path = PathBuf::from(p.get("path").and_then(|v| v.as_str()).unwrap_or(""));
    let estimated = p
        .get("estimated_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let desc = ModelDescriptor {
        id,
        format,
        path,
        estimated_memory_bytes: estimated,
        placement: ModelPlacementState::Dormant,
        resident_plane_id: None,
        last_accessed: chrono::Utc::now(),
        access_count: 0,
        preferred_plane: None,
    };
    arbiter.register_model(desc).await;
    VarlinkReply::ok(json!({}))
}

pub async fn handle_evict_model(params: Option<&Value>, arbiter: &Arc<Arbiter>) -> VarlinkReply {
    let p = match params {
        Some(v) => v,
        None => {
            return VarlinkReply::error(
                "org.varlink.service.InvalidParameter",
                json!({"parameter": "parameters"}),
            )
        }
    };
    let model_id = p
        .get("model")
        .or_else(|| p.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if model_id.is_empty() {
        return VarlinkReply::error(
            "org.varlink.service.InvalidParameter",
            json!({"parameter": "model"}),
        );
    }
    match arbiter.remove_model(model_id).await {
        Some(_) => VarlinkReply::ok(json!({})),
        None => VarlinkReply::error(
            "io.syntrop.Inference1.ModelNotFound",
            json!({"model": model_id}),
        ),
    }
}

pub async fn handle_pin_model(params: Option<&Value>, arbiter: &Arc<Arbiter>) -> VarlinkReply {
    let p = match params {
        Some(v) => v,
        None => {
            return VarlinkReply::error(
                "org.varlink.service.InvalidParameter",
                json!({"parameter": "parameters"}),
            )
        }
    };
    let model_id = p
        .get("model")
        .or_else(|| p.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let plane_id = p
        .get("plane")
        .or_else(|| p.get("plane_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if model_id.is_empty() || plane_id.is_empty() {
        return VarlinkReply::error(
            "org.varlink.service.InvalidParameter",
            json!({"parameter": if model_id.is_empty() { "model" } else { "plane" }}),
        );
    }
    if arbiter.pin_model_for_triage(model_id, plane_id.to_string()).await {
        VarlinkReply::ok(json!({}))
    } else {
        VarlinkReply::error(
            "io.syntrop.Inference1.ModelNotFound",
            json!({"model": model_id}),
        )
    }
}
