pub mod composite_leases;
pub mod inferenced1;
pub mod interface_descriptions;
pub mod leases;
pub mod models;
pub mod protocol;
pub mod service;
pub mod telemetry;

#[cfg(test)]
mod tests;

use inferenced_core::arbiter::Arbiter;
use inferenced_core::lease::LeaseId;
use protocol::{make_method_not_found, parse_request};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tracing::{error, info};

pub const DEFAULT_VARLINK_PATH: &str = "/run/syntrop/io.syntrop.Inference1";
#[allow(dead_code)]
pub const LEGACY_VARLINK_PATH: &str = "/run/systemd-inferenced/io.systemd.inferenced1";

pub async fn run_varlink_listener(
    listener: UnixListener,
    arbiter: Arc<Arbiter>,
) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let peer_info = inferenced_core::PeerInfo::from_socket(&stream).ok();
        let arbiter_clone = arbiter.clone();

        tokio::spawn(async move {
            let (reader, writer) = stream.into_split();
            let mut buf_reader = BufReader::new(reader);
            let mut writer_opt = Some(writer);
            let mut buf = Vec::new();
            let mut active_leases: Vec<LeaseId> = Vec::new();

            loop {
                buf.clear();
                let mut chunk = (&mut buf_reader).take(1024 * 1024);
                match chunk.read_until(0, &mut buf).await {
                    Ok(0) => break, // Client disconnected
                    Ok(_) => {
                        let req = match parse_request(&buf) {
                            Ok(r) => r,
                            Err(e) => {
                                error!("Varlink parse error: {}", e);
                                break;
                            }
                        };

                        let method = req.method.as_str();
                        let params = req.parameters.as_ref();

                        let maybe_reply = match method {
                            "org.varlink.service.GetInfo" => Some(service::handle_get_info()),
                            "org.varlink.service.GetInterfaceDescription" => {
                                Some(service::handle_get_interface_description(params))
                            }
                            _ if method.starts_with("io.syntrop.Telemetry1.") => {
                                telemetry::handle_telemetry_method(method)
                            }
                            _ => {
                                inferenced1::handle_method(
                                    method,
                                    params,
                                    &arbiter_clone,
                                    &mut writer_opt,
                                    &mut active_leases,
                                    peer_info.as_ref(),
                                )
                                .await
                            }
                        };

                        if let Some(reply) = maybe_reply {
                            if let Some(ref mut w) = writer_opt {
                                if let Err(e) = w.write_all(&reply.to_bytes()).await {
                                    error!("Failed to write Varlink reply: {}", e);
                                    break;
                                }
                            }
                        } else if writer_opt.is_some() && !method.contains("StreamInference") {
                            let not_found = make_method_not_found(method);
                            if let Some(ref mut w) = writer_opt {
                                let _ = w.write_all(&not_found).await;
                            }
                        }
                    }
                    Err(e) => {
                        error!("Varlink read error: {}", e);
                        break;
                    }
                }
            }

            // Auto-reclaim any lingering leases held by disconnected client
            for lease_id in active_leases {
                info!(
                    "Reclaiming orphaned lease {} from disconnected Varlink client",
                    lease_id
                );
                let _ = arbiter_clone.release_lease(lease_id).await;
            }
        });
    }
}

pub fn bind_or_create_listener(path_str: &str) -> anyhow::Result<UnixListener> {
    let path = Path::new(path_str);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    let listener = UnixListener::bind(path)?;
    info!("Bound Varlink IPC socket on {}", path_str);
    Ok(listener)
}
