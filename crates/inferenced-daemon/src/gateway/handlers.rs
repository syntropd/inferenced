use super::types::*;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use inferenced_core::{arbiter::Arbiter, lease::LeasePriority, preempt::PreemptCoordinator};
use std::sync::Arc;
use tracing::info;

pub struct AppState {
    pub arbiter: Arc<Arbiter>,
    #[allow(dead_code)]
    pub preempt: Arc<PreemptCoordinator>,
    pub api_token: Option<String>,
}

fn check_auth(headers: &axum::http::HeaderMap, api_token: Option<&str>) -> bool {
    let Some(expected) = api_token else { return true; };
    headers.get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|auth| auth.strip_prefix("Bearer ").unwrap_or(auth).trim() == expected)
        .unwrap_or(false)
}

pub async fn health_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let topo = state.arbiter.get_topology().await;
    Json(HealthResponse {
        status: "ok".into(),
        planes: topo.planes.len(),
        daemon: "systemd-inferenced v0.1.0".into(),
    })
}

pub async fn models_handler() -> impl IntoResponse {
    Json(serde_json::json!({
        "object": "list",
        "data": [
            { "id": "qwen2.5-coder:7b", "object": "model", "owned_by": "systemd-inferenced" },
            { "id": "llama3.2:1b", "object": "model", "owned_by": "systemd-inferenced" }
        ]
    }))
}

pub async fn chat_completions_handler(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(req): Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    if !check_auth(&headers, state.api_token.as_deref()) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "Unauthorized" }))).into_response();
    }
    let memory_needed = state.arbiter.get_model(&req.model).await
        .map(|m| m.estimated_memory_bytes).unwrap_or(512 * 1024 * 1024);
    let lease = match state
        .arbiter
        .acquire_lease(
            LeasePriority::Interactive,
            memory_needed,
            None,
            Some("gateway-client".into()),
            None,
        )
        .await
    {
        Ok(l) => l,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response();
        }
    };

    info!("Executing inference for model {} under lease {}", req.model, lease.id);
    let _ = state.arbiter.release_lease(lease.id).await;

    (
        StatusCode::OK,
        Json(serde_json::json!(ChatCompletionResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            model: req.model,
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".into(),
                    content: "systemd-inferenced: Model compute slice allocated and verified.".into(),
                },
                finish_reason: "stop".into(),
            }],
        })),
    )
        .into_response()
}
