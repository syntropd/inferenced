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
    topo.planes.push(
        ComputePlane::builder("plane-gpu-0")
            .name("Test GPU 0")
            .kind(ComputePlaneKind::DiscreteGpu)
            .no_device_path()
            .total_memory(8 * 1024 * 1024 * 1024)
            .numa_node(Some(0))
            .build(),
    );
    topo.planes.push(
        ComputePlane::builder("plane-gpu-1")
            .name("Test GPU 1")
            .kind(ComputePlaneKind::DiscreteGpu)
            .no_device_path()
            .total_memory(8 * 1024 * 1024 * 1024)
            .numa_node(Some(0))
            .build(),
    );
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

#[tokio::test]
async fn test_varlink_drm_watermark_and_resize_lease() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("varlink_watermark_test.sock");
    let listener = bind_or_create_listener(sock.to_str().unwrap()).unwrap();
    let arbiter = Arc::new(Arbiter::new(make_two_gpu_topo()));

    let s_arb = arbiter.clone();
    tokio::spawn(async move {
        let _ = run_varlink_listener(listener, s_arb).await;
    });

    let mut client = UnixStream::connect(&sock).await.unwrap();

    // 1. GetDrmWatermark
    let wm_resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.GetDrmWatermark",
        json!({}),
    ).await;
    assert!(wm_resp["parameters"]["watermarks"].is_array());

    // 2. AcquireLease
    let acq_resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.AcquireLease",
        json!({
            "priority": "Interactive",
            "memory_bytes": 1024 * 1024 * 1024,
            "plane": "plane-gpu-0",
        }),
    ).await;
    let lease_id = acq_resp["parameters"]["lease_id"].as_str().unwrap();

    // 3. ResizeLease to 2GB
    let resize_resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.ResizeLease",
        json!({
            "lease_id": lease_id,
            "memory_bytes": 2u64 * 1024 * 1024 * 1024,
        }),
    ).await;
    assert_eq!(resize_resp["parameters"]["lease_id"], lease_id);
    assert_eq!(resize_resp["parameters"]["allocated_memory"], 2u64 * 1024 * 1024 * 1024);

    // 3b. ResizeLease exceeding plane capacity -> ResourceExhaustion
    let oom_resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.ResizeLease",
        json!({
            "lease_id": lease_id,
            "memory_bytes": 100u64 * 1024 * 1024 * 1024,
        }),
    ).await;
    assert_eq!(oom_resp["error"], "io.syntrop.Inference1.ResourceExhaustion");

    // 3c. ResizeLease with unknown lease_id -> LeaseNotFound
    let notfound_resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.ResizeLease",
        json!({
            "lease_id": uuid::Uuid::new_v4().to_string(),
            "memory_bytes": 1024 * 1024 * 1024,
        }),
    ).await;
    assert_eq!(notfound_resp["error"], "io.syntrop.Inference1.LeaseNotFound");

    // 3d. ResizeLease shrink to 512MB
    let shrink_resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.ResizeLease",
        json!({
            "lease_id": lease_id,
            "memory_bytes": 512u64 * 1024 * 1024,
        }),
    ).await;
    assert_eq!(shrink_resp["parameters"]["allocated_memory"], 512u64 * 1024 * 1024);

    // 3e. Separate client connection cannot resize lease owned by another connection
    let mut client2 = UnixStream::connect(&sock).await.unwrap();
    let unowned_resp = varlink_call(
        &mut client2,
        "io.syntrop.Inference1.ResizeLease",
        json!({
            "lease_id": lease_id,
            "memory_bytes": 1024u64 * 1024 * 1024,
        }),
    ).await;
    assert_eq!(unowned_resp["error"], "io.syntrop.Inference1.LeaseNotFound");

    // 4. ReleaseLease
    let rel_resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.ReleaseLease",
        json!({ "lease_id": lease_id }),
    ).await;
    assert!(rel_resp["parameters"].is_object());
}

#[tokio::test]
async fn test_varlink_acquire_lease_hardware_incompatible_cpu_only() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("varlink_compat_test.sock");
    let listener = bind_or_create_listener(sock.to_str().unwrap()).unwrap();
    let topo = HardwareTopology::default();
    let arbiter = Arc::new(Arbiter::new(topo));

    tokio::spawn(async move {
        let _ = run_varlink_listener(listener, arbiter).await;
    });

    let mut client = UnixStream::connect(&sock).await.unwrap();
    let resp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.AcquireLease",
        json!({
            "priority": "Interactive",
            "memory_bytes": 1024 * 1024 * 1024,
            "workload": "VideoTemporal"
        }),
    ).await;

    assert_eq!(resp["error"], "io.syntrop.Inference1.HardwareIncompatible");
    assert!(resp["parameters"]["deficit"].as_str().unwrap().contains("discrete GPU VRAM"));
    assert!(!resp["parameters"]["suggested_alternatives"].as_array().unwrap().is_empty());

    let resp_comp = varlink_call(
        &mut client,
        "io.syntrop.Inference1.AcquireCompositeLease",
        json!({
            "priority": "Interactive",
            "slices": [{ "role": "Primary", "memory_bytes": 1024 * 1024 * 1024 }],
            "workload": "VideoTemporal"
        }),
    ).await;
    assert_eq!(resp_comp["error"], "io.syntrop.Inference1.HardwareIncompatible");
}
