use crate::varlink::{bind_or_create_listener, run_varlink_listener};
use inferenced_core::{
    arbiter::Arbiter,
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use serde_json::{json, Value};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn make_two_gpu_topo() -> HardwareTopology {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-gpu-0".into(),
        name: "Test GPU 0".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: 8 * 1024 * 1024 * 1024,
        available_memory_bytes: 8 * 1024 * 1024 * 1024,
        numa_node: Some(0),
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
    });
    topo.planes.push(ComputePlane {
        id: "plane-gpu-1".into(),
        name: "Test GPU 1".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: 8 * 1024 * 1024 * 1024,
        available_memory_bytes: 8 * 1024 * 1024 * 1024,
        numa_node: Some(0),
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
    });
    topo
}

async fn varlink_call(stream: &mut UnixStream, method: &str, params: Value) -> Value {
    let req = json!({ "method": method, "parameters": params });
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
async fn test_varlink_acquire_and_release_composite_lease() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("varlink_gang_test.sock");
    let listener = bind_or_create_listener(sock.to_str().unwrap()).unwrap();
    let arbiter = Arc::new(Arbiter::new(make_two_gpu_topo()));

    let s_arb = arbiter.clone();
    tokio::spawn(async move {
        let _ = run_varlink_listener(listener, s_arb).await;
    });

    let mut client = UnixStream::connect(&sock).await.unwrap();

    let acq_resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.AcquireCompositeLease",
        json!({
            "priority": "Interactive",
            "policy": "AllOrNothing",
            "slices": [
                { "role": "Primary", "memory_bytes": 2u64 * 1024 * 1024 * 1024, "plane": "plane-gpu-0" },
                { "role": "Worker", "memory_bytes": 2u64 * 1024 * 1024 * 1024, "plane": "plane-gpu-1" }
            ]
        }),
    ).await;

    let lease_id = acq_resp["parameters"]["lease_id"].as_str().unwrap();
    assert!(!lease_id.is_empty());
    let slices = acq_resp["parameters"]["slices"].as_array().unwrap();
    assert_eq!(slices.len(), 2);

    let list_resp = varlink_call(&mut client, "io.syntrop.Inference1.ListCompositeLeases", json!({})).await;
    let list = list_resp["parameters"]["composite_leases"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["lease_id"], lease_id);

    let rel_resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.ReleaseCompositeLease",
        json!({ "lease_id": lease_id }),
    ).await;
    assert!(rel_resp["parameters"].is_object());
}
