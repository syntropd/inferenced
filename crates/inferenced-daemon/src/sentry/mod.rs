use inferenced_core::{arbiter::Arbiter, lease::LeasePriority};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tracing::{error, info, warn};

#[allow(dead_code)]
pub const DEFAULT_SENTRY_SOCKET_PATH: &str = "/run/systemd-inferenced/sentry.sock";

#[derive(Debug, Serialize, Deserialize)]
pub struct SentryTriageResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incident_id: Option<String>,
    pub allocated_plane: String,
    pub plane_assigned: String,
    pub queue_latency_us: u64,
    pub preemption_triggered: bool,
    pub analysis: String,
}

pub async fn run_sentry_triage_listener(
    listener: UnixListener,
    arbiter: Arc<Arbiter>,
) -> anyhow::Result<()> {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let arbiter_clone = arbiter.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_sentry_connection(stream, arbiter_clone).await {
                        error!("Error handling sentry triage connection: {}", e);
                    }
                });
            }
            Err(e) => {
                warn!("Error accepting triage connection: {}", e);
            }
        }
    }
}

async fn handle_sentry_connection(
    mut stream: UnixStream,
    arbiter: Arc<Arbiter>,
) -> anyhow::Result<()> {
    info!("Received emergency triage connection from systemd-sentry!");

    let mut buf = vec![0u8; 65536];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }

        let payload_json: Value = serde_json::from_slice(&buf[..n])
            .unwrap_or_else(|_| serde_json::json!({ "raw": String::from_utf8_lossy(&buf[..n]) }));

        let incident_id = payload_json
            .get("incident_id")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);

        let unit_name = payload_json
            .get("unit_name")
            .and_then(|v| v.as_str())
            .unwrap_or("systemd-sentry.service");

        // Acquire emergency triage slice on protected plane
        let lease = arbiter
            .acquire_lease(
                LeasePriority::EmergencyTriage,
                1024 * 1024 * 1024, // 1GB dedicated emergency memory
                None,
                Some(unit_name.to_string()),
                None,
            )
            .await?;

        info!(
            "Acquired emergency lease {} on plane {} for sentry incident {:?}",
            lease.id, lease.plane_id, incident_id
        );

        let response = SentryTriageResponse {
            status: "accepted".into(),
            incident_id,
            allocated_plane: lease.plane_id.clone(),
            plane_assigned: lease.plane_id.clone(),
            queue_latency_us: 0,
            preemption_triggered: true,
            analysis: format!(
                "Emergency compute isolated on plane {}. AI hardware state preserved.",
                lease.plane_id
            ),
        };

        let reply_bytes = serde_json::to_vec(&response)?;
        let write_res = stream.write_all(&reply_bytes).await.and(stream.flush().await);
        let _ = arbiter.release_lease(lease.id).await;
        write_res?;
        info!("Released emergency triage lease {}", lease.id);
    }

    Ok(())
}

pub fn bind_or_create_sentry_listener(path: &Path) -> anyhow::Result<UnixListener> {
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(path)?;
    info!("Bound Sentry emergency triage listener on {:?}", path);
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;
    use inferenced_core::topology::{ComputePlane, ComputePlaneKind, HardwareTopology};
    use tempfile::tempdir;

    fn make_test_topology() -> HardwareTopology {
        let mut topo = HardwareTopology::default();
        topo.planes.push(
            ComputePlane::builder("npu-sentry-0")
                .name("Protected Sentry NPU Enclave")
                .kind(ComputePlaneKind::NpuAccelerator)
                .no_device_path()
                .total_memory(4 * 1024 * 1024 * 1024)
                .is_triage_reserved(true)
                .build(),
        );
        topo
    }

    #[tokio::test]
    async fn test_sentry_enclave_triage_ping_roundtrip() {
        let dir = tempdir().unwrap();
        let sock_path = dir.path().join("sentry_roundtrip.sock");
        let listener = bind_or_create_sentry_listener(&sock_path).unwrap();
        let arbiter = Arc::new(Arbiter::new(make_test_topology()));

        let server_arbiter = arbiter.clone();
        tokio::spawn(async move {
            let _ = run_sentry_triage_listener(listener, server_arbiter).await;
        });

        let mut client = UnixStream::connect(&sock_path).await.unwrap();
        let ping_req = serde_json::json!({
            "action": "triage_ping",
            "incident_id": "panic-gpu-hang-007",
            "unit_name": "systemd-sentry.service"
        });
        client
            .write_all(&serde_json::to_vec(&ping_req).unwrap())
            .await
            .unwrap();

        let mut resp_buf = vec![0u8; 1024];
        let n = client.read(&mut resp_buf).await.unwrap();
        assert!(n > 0);

        let resp: SentryTriageResponse = serde_json::from_slice(&resp_buf[..n]).unwrap();
        assert_eq!(resp.status, "accepted");
        assert_eq!(resp.allocated_plane, "npu-sentry-0");
        assert_eq!(resp.plane_assigned, "npu-sentry-0");
        assert_eq!(resp.incident_id, Some("panic-gpu-hang-007".into()));
        assert!(resp.preemption_triggered);
    }
}
