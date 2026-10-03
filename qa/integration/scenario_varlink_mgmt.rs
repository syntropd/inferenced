use inferenced_core::{
    arbiter::Arbiter,
    lease::LeasePriority,
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use serde_json::{json, Value};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

#[tokio::test]
async fn test_scenario_varlink_service_introspection_and_management() {
    // Scenario 4: Full Varlink service lifecycle: org.varlink.service.GetInfo,
    // interface inspection, topology discovery, lease acquisition, and release.
    let dir = tempdir().unwrap();
    let socket_path = dir.path().join("varlink_mgmt.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let mut topo = HardwareTopology::default();
    topo.planes.push(
        ComputePlane::builder("mgmt-plane-0")
            .name("Enterprise NPU Accelerator")
            .kind(ComputePlaneKind::NpuAccelerator)
            .no_device_path()
            .total_memory(8 * 1024 * 1024 * 1024)
            .numa_node(None)
            .supported_formats(vec!["GGUF".into()])
            .hardware_features(vec!["Hailo-8".into()])
            .build(),
    );

    let arbiter = Arc::new(Arbiter::new(topo));
    let server_arbiter = arbiter.clone();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut buf_reader = BufReader::new(reader);
        let mut line = Vec::new();

        while let Ok(n) = buf_reader.read_until(0, &mut line).await {
            if n == 0 { break; }
            let req: Value = serde_json::from_slice(&line[..line.len() - 1]).unwrap();
            let method = req["method"].as_str().unwrap();

            let reply = match method {
                "org.varlink.service.GetInfo" => json!({
                    "parameters": {
                        "vendor": "systemd-inferenced",
                        "product": "systemd-inferenced",
                        "version": "0.1.0",
                        "url": "https://github.com/syntropd/inferenced",
                        "interfaces": ["org.varlink.service", "io.syntrop.Inference1"]
                    }
                }),
                "io.syntrop.Inference1.GetTopology" => {
                    let t = server_arbiter.get_topology().await;
                    json!({
                        "parameters": {
                            "planes": [{
                                "id": t.planes[0].id,
                                "name": t.planes[0].name,
                                "total_memory": t.planes[0].total_memory_bytes,
                                "available_memory": t.planes[0].available_memory_bytes,
                            }]
                        }
                    })
                },
                "io.syntrop.Inference1.AcquireLease" => {
                    let mem = req["parameters"]["memory_bytes"].as_u64().unwrap();
                    let l = server_arbiter
                        .acquire_lease(LeasePriority::Interactive, mem, None, None, None)
                        .await
                        .unwrap();
                    json!({
                        "parameters": {
                            "lease_id": l.id.to_string(),
                            "plane_id": l.plane_id
                        }
                    })
                },
                "io.syntrop.Inference1.ReleaseLease" => {
                    let id_str = req["parameters"]["lease_id"].as_str().unwrap();
                    let uid = uuid::Uuid::parse_str(id_str).unwrap();
                    server_arbiter.release_lease(inferenced_core::lease::LeaseId(uid)).await.unwrap();
                    json!({ "parameters": {} })
                },
                _ => json!({ "error": "org.varlink.service.MethodNotFound" }),
            };

            let mut b = serde_json::to_vec(&reply).unwrap();
            b.push(0);
            writer.write_all(&b).await.unwrap();
            line.clear();
        }
    });

    let stream = UnixStream::connect(&socket_path).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    // Helper closure to send and receive Varlink frame
    let call = |method: &str, params: Value| {
        let req = json!({ "method": method, "parameters": params });
        let mut b = serde_json::to_vec(&req).unwrap();
        b.push(0);
        b
    };

    // 1. varlinkctl info equivalent: GetInfo
    writer.write_all(&call("org.varlink.service.GetInfo", json!({}))).await.unwrap();
    let mut resp_buf = Vec::new();
    buf_reader.read_until(0, &mut resp_buf).await.unwrap();
    let info: Value = serde_json::from_slice(&resp_buf[..resp_buf.len() - 1]).unwrap();
    assert_eq!(info["parameters"]["vendor"], "systemd-inferenced");
    assert!(info["parameters"]["interfaces"].as_array().unwrap().contains(&json!("io.syntrop.Inference1")));

    // 2. Discover compute planes via Varlink
    resp_buf.clear();
    writer.write_all(&call("io.syntrop.Inference1.GetTopology", json!({}))).await.unwrap();
    buf_reader.read_until(0, &mut resp_buf).await.unwrap();
    let topo_resp: Value = serde_json::from_slice(&resp_buf[..resp_buf.len() - 1]).unwrap();
    assert_eq!(topo_resp["parameters"]["planes"][0]["id"], "mgmt-plane-0");

    // 3. Acquire lease via Varlink
    resp_buf.clear();
    writer.write_all(&call("io.syntrop.Inference1.AcquireLease", json!({ "memory_bytes": 1024 * 1024 * 1024 }))).await.unwrap();
    buf_reader.read_until(0, &mut resp_buf).await.unwrap();
    let lease_resp: Value = serde_json::from_slice(&resp_buf[..resp_buf.len() - 1]).unwrap();
    let lease_id = lease_resp["parameters"]["lease_id"].as_str().unwrap().to_string();

    // 4. Release lease via Varlink
    resp_buf.clear();
    writer.write_all(&call("io.syntrop.Inference1.ReleaseLease", json!({ "lease_id": lease_id }))).await.unwrap();
    buf_reader.read_until(0, &mut resp_buf).await.unwrap();
    let release_resp: Value = serde_json::from_slice(&resp_buf[..resp_buf.len() - 1]).unwrap();
    assert!(release_resp.get("error").is_none());
}
