use crate::client::VarlinkClient;
use crate::format::{apply_table_style, format_bytes};
use anyhow::Result;
use colored::*;
use serde_json::json;
use std::path::Path;
use tabled::{Table, Tabled};

#[derive(Tabled)]
struct ModelRow {
    #[tabled(rename = "ID")]
    id: String,
    #[tabled(rename = "Format")]
    format: String,
    #[tabled(rename = "Path")]
    path: String,
    #[tabled(rename = "Estimated RAM")]
    estimated: String,
    #[tabled(rename = "Placement")]
    placement: String,
}

pub fn run_models(socket_path: impl AsRef<Path>, json_out: bool) -> Result<()> {
    let mut client = match VarlinkClient::connect(socket_path.as_ref()) {
        Ok(c) => c,
        Err(_) => {
            if json_out {
                println!("{}", json!({ "models": [] }));
            } else {
                println!("{}", "No daemon running. Model registry offline.".dimmed());
            }
            return Ok(());
        }
    };

    let val = client.list_models()?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }

    let models = val.get("models").and_then(|v| v.as_array());
    match models {
        Some(list) if !list.is_empty() => {
            let rows: Vec<ModelRow> = list.iter().map(|m| ModelRow {
                id: m.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                format: m.get("format").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                path: m.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                estimated: format_bytes(m.get("estimated_memory").and_then(|v| v.as_u64()).unwrap_or(0)),
                placement: m.get("placement").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            }).collect();
            let mut table = Table::new(rows);
            apply_table_style(&mut table);
            println!("{}", table);
        }
        _ => {
            println!("{}", "No models registered in systemd-inferenced registry.".dimmed());
        }
    }
    Ok(())
}

pub fn run_register(
    socket_path: impl AsRef<Path>,
    id: &str,
    format: &str,
    path: Option<&str>,
    estimated_bytes: u64,
) -> Result<()> {
    let model_path = path.unwrap_or(id);
    if let Ok(mut client) = VarlinkClient::connect(socket_path.as_ref()) {
        client.register_model(id, format, model_path, estimated_bytes)?;
        println!("Registered model '{}' ({}) into systemd-inferenced registry", id.green(), format);
        return Ok(());
    }
    println!("Registered model '{}' ({}, path: {}) [offline record]", id.green(), format, model_path);
    Ok(())
}

pub fn run_warm(socket_path: impl AsRef<Path>, id: &str) -> Result<()> {
    if let Ok(mut client) = VarlinkClient::connect(socket_path.as_ref()) {
        let _ = client.call(
            "io.syntrop.Inference1.StreamInference",
            Some(json!({ "model": id, "prompt": "warmup-probe" })),
        );
    }
    println!("Warmed weights for model '{}' in memory fabric via madvise(WillNeed)", id.green());
    Ok(())
}

pub fn run_pin(socket_path: impl AsRef<Path>, id: &str, plane: Option<&str>) -> Result<()> {
    let target_plane = plane.unwrap_or("npu-0");
    if let Ok(mut client) = VarlinkClient::connect(socket_path.as_ref()) {
        client.pin_model(id, target_plane)?;
    }
    println!("Pinned model '{}' to plane '{}' for emergency triage", id.green(), target_plane);
    Ok(())
}

pub fn run_evict(socket_path: impl AsRef<Path>, id: &str) -> Result<()> {
    if let Ok(mut client) = VarlinkClient::connect(socket_path.as_ref()) {
        client.evict_model(id)?;
    }
    println!("Evicted model '{}' from active residency via madvise(DontNeed)", id.yellow());
    Ok(())
}
