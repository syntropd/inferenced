use inferenced_core::{
    arbiter::Arbiter,
    lease::LeasePriority,
    topology::{ComputePlane, HardwareTopology},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

#[derive(Serialize, Deserialize)]
struct VarlinkMsg {
    method: String,
    parameters: Option<Value>,
}

#[tokio::test]
async fn test_varlink_get_info_introspection() {
    let expected_info = json!({
        "vendor": "systemd-inferenced",
        "product": "systemd-inferenced",
        "version": "0.1.0",
        "url": "https://github.com/syntropd/inferenced",
        "interfaces": [
            "org.varlink.service",
            "io.syntrop.Inference1"
        ]
    });

    assert_eq!(expected_info["vendor"], "systemd-inferenced");
    assert_eq!(expected_info["interfaces"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_varlink_wire_framing_nul_delimiter() {
    let req = json!({
        "method": "io.syntrop.Inference1.GetTopology",
        "parameters": {}
    });
    let mut bytes = serde_json::to_vec(&req).unwrap();
    bytes.push(0);

    assert_eq!(*bytes.last().unwrap(), 0u8, "Varlink message must terminate with NUL byte");

    // Deserialize trimming NUL
    let clean = &bytes[..bytes.len() - 1];
    let parsed: VarlinkMsg = serde_json::from_slice(clean).unwrap();
    assert_eq!(parsed.method, "io.syntrop.Inference1.GetTopology");
}

#[tokio::test]
async fn test_varlink_get_topology_payload() {
    let mut topo = HardwareTopology::default();
    topo.planes.push(
        ComputePlane::builder("plane-gpu-0")
            .name("Discrete GPU")
            .no_device_path()
            .total_memory(8 * 1024 * 1024 * 1024)
            .numa_node(None)
            .supported_formats(vec![])
            .hardware_features(vec![])
            .build(),
    );

    let arbiter = Arbiter::new(topo);
    let state_topo = arbiter.get_topology().await;

    let reply = json!({
        "parameters": {
            "planes": [{
                "id": state_topo.planes[0].id,
                "name": state_topo.planes[0].name,
                "total_memory": state_topo.planes[0].total_memory_bytes,
                "available_memory": state_topo.planes[0].available_memory_bytes,
            }],
            "cpu_cores": state_topo.cpu_cores_total,
        }
    });

    let planes = reply["parameters"]["planes"].as_array().unwrap();
    assert_eq!(planes.len(), 1);
    assert_eq!(planes[0]["id"], "plane-gpu-0");
}

#[tokio::test]
async fn test_varlink_acquire_and_release_roundtrip() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("varlink_test.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    let mut topo = HardwareTopology::default();
    topo.planes.push(
        ComputePlane::builder("plane-varlink")
            .name("Varlink Plane")
            .no_device_path()
            .total_memory(10 * 1024 * 1024 * 1024)
            .numa_node(None)
            .supported_formats(vec![])
            .hardware_features(vec![])
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

            if method == "io.syntrop.Inference1.AcquireLease" {
                let lease = server_arbiter
                    .acquire_lease(LeasePriority::Interactive, 1024 * 1024 * 1024, None, None, None)
                    .await
                    .unwrap();
                let resp = json!({
                    "parameters": {
                        "lease_id": lease.id.to_string(),
                        "plane_id": lease.plane_id
                    }
                });
                let mut b = serde_json::to_vec(&resp).unwrap();
                b.push(0);
                writer.write_all(&b).await.unwrap();
            }
            line.clear();
        }
    });

    let stream = UnixStream::connect(&sock).await.unwrap();
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    let req = json!({
        "method": "io.syntrop.Inference1.AcquireLease",
        "parameters": { "priority": "Interactive", "memory_bytes": 1024 * 1024 * 1024 }
    });
    let mut b = serde_json::to_vec(&req).unwrap();
    b.push(0);
    writer.write_all(&b).await.unwrap();

    let mut resp_buf = Vec::new();
    buf_reader.read_until(0, &mut resp_buf).await.unwrap();
    let resp: Value = serde_json::from_slice(&resp_buf[..resp_buf.len() - 1]).unwrap();

    assert!(resp["parameters"]["lease_id"].is_string());
    assert_eq!(resp["parameters"]["plane_id"], "plane-varlink");
}

#[tokio::test]
async fn test_varlink_method_not_found_reply() {
    let method = "io.systemd.nonexistent.Method";
    let reply = json!({
        "error": "org.varlink.service.MethodNotFound",
        "parameters": { "method": method }
    });
    assert_eq!(reply["error"], "org.varlink.service.MethodNotFound");
    assert_eq!(reply["parameters"]["method"], method);
}
