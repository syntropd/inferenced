use super::*;
mod composite_tests;
use inferenced_core::topology::{ComputePlane, ComputePlaneKind, HardwareTopology};
use serde_json::{json, Value};
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn make_test_topo() -> HardwareTopology {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-varlink-test".into(),
        name: "Test Accelerator".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: 8 * 1024 * 1024 * 1024,
        available_memory_bytes: 8 * 1024 * 1024 * 1024,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec!["vulkan".into()],
    });
    topo.total_system_ram_bytes = 32 * 1024 * 1024 * 1024;
    topo.available_system_ram_bytes = 16 * 1024 * 1024 * 1024;
    topo.cpu_cores_total = 8;
    topo
}

async fn varlink_call(stream: &mut UnixStream, method: &str, params: Value) -> Value {
    let req = json!({
        "method": method,
        "parameters": params,
    });
    let mut bytes = serde_json::to_vec(&req).unwrap();
    bytes.push(0);
    stream.write_all(&bytes).await.unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    reader.read_until(0, &mut line).await.unwrap();
    if let Some(&0) = line.last() {
        line.pop();
    }
    serde_json::from_slice(&line).unwrap()
}

#[tokio::test]
async fn test_varlink_server_get_status_and_list_planes() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("varlink_test.sock");
    let listener = bind_or_create_listener(sock.to_str().unwrap()).unwrap();
    let arbiter = Arc::new(Arbiter::new(make_test_topo()));

    let s_arb = arbiter.clone();
    tokio::spawn(async move {
        let _ = run_varlink_listener(listener, s_arb).await;
    });

    let mut client = UnixStream::connect(&sock).await.unwrap();

    // 1. GetInfo
    let info_resp = varlink_call(&mut client, "org.varlink.service.GetInfo", json!({})).await;
    assert_eq!(info_resp["parameters"]["product"], "inferenced");
    assert!(info_resp["parameters"]["interfaces"]
        .as_array()
        .unwrap()
        .contains(&json!("io.systemd.inferenced1")));

    // 2. GetInterfaceDescription
    let idl_resp = varlink_call(
        &mut client,
        "org.varlink.service.GetInterfaceDescription",
        json!({ "interface": "io.systemd.inferenced1" }),
    )
    .await;
    let desc = idl_resp["parameters"]["description"].as_str().unwrap();
    assert!(desc.contains("interface io.systemd.inferenced1"));
    assert!(desc.contains("method GetStatus()"));
    assert!(desc.contains("method ListPlanes()"));

    // 3. GetStatus
    let status_resp = varlink_call(&mut client, "io.systemd.inferenced1.GetStatus", json!({})).await;
    assert_eq!(status_resp["parameters"]["status"], "active");
    assert_eq!(status_resp["parameters"]["planes_count"], 1);

    // 4. ListPlanes
    let planes_resp = varlink_call(&mut client, "io.systemd.inferenced1.ListPlanes", json!({})).await;
    let planes = planes_resp["parameters"]["planes"].as_array().unwrap();
    assert_eq!(planes.len(), 1);
    assert_eq!(planes[0]["id"], "plane-varlink-test");
    assert_eq!(planes[0]["features"], json!(["vulkan"]));

    // 5. AcquireLease & ReleaseLease
    let acq_resp = varlink_call(
        &mut client,
        "io.systemd.inferenced1.AcquireLease",
        json!({
            "priority": "Interactive",
            "memory_bytes": 1024 * 1024 * 1024,
        }),
    )
    .await;
    let lease_id = acq_resp["parameters"]["lease_id"].as_str().unwrap();
    assert!(!lease_id.is_empty());

    // 6. FreezeLease and ThawLease
    let freeze_resp = varlink_call(
        &mut client,
        "io.systemd.inferenced1.FreezeLease",
        json!({ "lease_id": lease_id }),
    )
    .await;
    assert!(freeze_resp["parameters"].is_object());

    let thaw_resp = varlink_call(
        &mut client,
        "io.systemd.inferenced1.ThawLease",
        json!({ "lease_id": lease_id }),
    )
    .await;
    assert!(thaw_resp["parameters"].is_object());

    // 7. RegisterModel, ListModels, PinModel, EvictModel
    let reg_resp = varlink_call(
        &mut client,
        "io.systemd.inferenced1.RegisterModel",
        json!({
            "id": "model-llama-test",
            "format": "GGUF",
            "path": "/models/llama.gguf",
            "estimated_bytes": 4294967296u64,
        }),
    )
    .await;
    assert!(reg_resp["parameters"].is_object());

    let list_models_resp = varlink_call(&mut client, "io.systemd.inferenced1.ListModels", json!({})).await;
    let models = list_models_resp["parameters"]["models"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["id"], "model-llama-test");

    let pin_resp = varlink_call(
        &mut client,
        "io.systemd.inferenced1.PinModel",
        json!({
            "id": "model-llama-test",
            "plane": "plane-varlink-test",
        }),
    )
    .await;
    assert!(pin_resp["parameters"].is_object());

    let evict_resp = varlink_call(
        &mut client,
        "io.systemd.inferenced1.EvictModel",
        json!({ "id": "model-llama-test" }),
    )
    .await;
    assert!(evict_resp["parameters"].is_object());

    // 8. ReleaseLease
    let rel_resp = varlink_call(
        &mut client,
        "io.systemd.inferenced1.ReleaseLease",
        json!({ "lease_id": lease_id }),
    )
    .await;
    assert!(rel_resp["parameters"].is_object());
}

#[tokio::test]
async fn test_varlink_server_io_syntrop_inference1_telemetry_and_pressure() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("varlink_telemetry_test.sock");
    let listener = bind_or_create_listener(sock.to_str().unwrap()).unwrap();
    let arbiter = Arc::new(Arbiter::new(make_test_topo()));

    let s_arb = arbiter.clone();
    tokio::spawn(async move {
        let _ = run_varlink_listener(listener, s_arb).await;
    });

    let mut client = UnixStream::connect(&sock).await.unwrap();

    // 1. GetInterfaceDescription for io.syntrop.Inference1
    let idl_resp = varlink_call(
        &mut client,
        "org.varlink.service.GetInterfaceDescription",
        json!({ "interface": "io.syntrop.Inference1" }),
    )
    .await;
    let desc = idl_resp["parameters"]["description"].as_str().unwrap();
    assert!(desc.contains("interface io.syntrop.Inference1"));
    assert!(desc.contains("method GetPressure()"));
    assert!(desc.contains("method GetTopology()"));

    // 2. io.syntrop.Inference1.GetPressure
    let pressure_resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.GetPressure",
        json!({}),
    )
    .await;
    let params = &pressure_resp["parameters"];
    assert!(params["level"].is_string());
    assert!(params["cpu_some"].is_number());
    assert!(params["memory_some"].is_number());
    assert!(params["io_some"].is_number());

    // 3. io.syntrop.Inference1.GetTopology
    let topo_resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.GetTopology",
        json!({}),
    )
    .await;
    let topo_params = &topo_resp["parameters"];
    let planes = topo_params["planes"].as_array().unwrap();
    assert_eq!(planes.len(), 1);
    assert_eq!(planes[0]["id"], "plane-varlink-test");
    assert_eq!(planes[0]["features"], json!(["vulkan"]));
    assert!(topo_params["total_ram"].as_u64().unwrap() > 0);
    assert!(topo_params["cpu_cores"].as_u64().unwrap() > 0);

    // 4. io.syntrop.Inference1.GetStatus
    let status_resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.GetStatus",
        json!({}),
    )
    .await;
    assert_eq!(status_resp["parameters"]["status"], "active");
    assert!(status_resp["parameters"]["pressure"].is_string());
}
