use inferenced_core::{
    arbiter::Arbiter,
    lease::{LeaseId, LeasePriority, LeaseState},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use serde_json::{json, Value};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

fn create_rogue_topology(total_mem: u64) -> HardwareTopology {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-rogue-gpu".into(),
        name: "Rogue Test GPU Plane".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: total_mem,
        available_memory_bytes: total_mem,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
    });
    topo
}

async fn run_rogue_varlink_server(listener: UnixListener, arbiter: Arc<Arbiter>) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => break,
        };
        let arb = arbiter.clone();
        tokio::spawn(async move {
            let (reader, mut writer) = stream.into_split();
            let mut buf_reader = BufReader::new(reader);
            let mut active_leases: Vec<LeaseId> = Vec::new();
            let mut buf = Vec::new();

            loop {
                buf.clear();
                match buf_reader.read_until(0, &mut buf).await {
                    Ok(0) => break,
                    Ok(_) => {
                        let parsed: Result<Value, _> = serde_json::from_slice(&buf[..buf.len() - 1]);
                        let req = match parsed {
                            Ok(v) => v,
                            Err(_) => break,
                        };
                        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
                        let params = req.get("parameters");

                        match method {
                            "io.systemd.inferenced1.AcquireLease" => {
                                let prio_str = params
                                    .and_then(|p| p.get("priority"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("Interactive");
                                let prio = match prio_str {
                                    "Batch" => LeasePriority::Batch,
                                    "EmergencyTriage" => LeasePriority::EmergencyTriage,
                                    _ => LeasePriority::Interactive,
                                };
                                let mem = params
                                    .and_then(|p| p.get("memory_bytes"))
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(1024 * 1024 * 1024);

                                match arb.acquire_lease(prio, mem, Some("plane-rogue-gpu".into()), None, None).await {
                                    Ok(lease) => {
                                        active_leases.push(lease.id);
                                        let rep = json!({
                                            "parameters": {
                                                "lease_id": lease.id.to_string(),
                                                "plane_id": lease.plane_id
                                            }
                                        });
                                        let mut b = serde_json::to_vec(&rep).unwrap();
                                        b.push(0);
                                        if writer.write_all(&b).await.is_err() { break; }
                                    }
                                    Err(_) => break,
                                }
                            }
                            "io.systemd.inferenced1.StreamInference" => {
                                for i in 0..5 {
                                    let chunk = json!({
                                        "parameters": { "chunk": format!("token_{} ", i) },
                                        "continues": i < 4
                                    });
                                    let mut b = serde_json::to_vec(&chunk).unwrap();
                                    b.push(0);
                                    if writer.write_all(&b).await.is_err() { break; }
                                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                                }
                            }
                            _ => break,
                        }
                    }
                    Err(_) => break,
                }
            }

            for lease_id in active_leases {
                let _ = arb.release_lease(lease_id).await;
            }
        });
    }
}

#[tokio::test]
async fn test_rogue_client_disconnect_reclaims_all_leases_and_memory() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("rogue_test.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    let total_mem = 32 * 1024 * 1024 * 1024; // 32GB
    let arbiter = Arc::new(Arbiter::new(create_rogue_topology(total_mem)));

    let srv_arbiter = arbiter.clone();
    let srv_handle = tokio::spawn(async move {
        run_rogue_varlink_server(listener, srv_arbiter).await;
    });

    // Wave 1: 10 clients acquire 1GB lease, initiate streaming inference, then abruptly drop socket
    for i in 0..10 {
        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let req = json!({
            "method": "io.systemd.inferenced1.AcquireLease",
            "parameters": { "priority": "Interactive", "memory_bytes": 1024 * 1024 * 1024 }
        });
        let mut b = serde_json::to_vec(&req).unwrap();
        b.push(0);
        stream.write_all(&b).await.unwrap();

        let mut buf_reader = BufReader::new(&mut stream);
        let mut resp_buf = Vec::new();
        buf_reader.read_until(0, &mut resp_buf).await.unwrap();
        let resp: Value = serde_json::from_slice(&resp_buf[..resp_buf.len() - 1]).unwrap();
        assert!(resp["parameters"]["lease_id"].is_string());

        let stream_req = json!({
            "method": "io.systemd.inferenced1.StreamInference",
            "parameters": { "prompt": format!("rogue-prompt-{}", i) }
        });
        let mut sb = serde_json::to_vec(&stream_req).unwrap();
        sb.push(0);
        let _ = stream.write_all(&sb).await;

        drop(stream);
    }

    // Wave 2: 10 clients send corrupted/partial Varlink frames and drop socket
    for _ in 0..10 {
        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let _ = stream.write_all(b"{\"method\": \"invalid-malformed-json...").await;
        drop(stream);
    }

    // Wave 3: 10 clients acquire 1GB Batch leases, get preempted, then drop socket while Preempted
    let mut batch_clients = Vec::new();
    for _ in 0..10 {
        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let req = json!({
            "method": "io.systemd.inferenced1.AcquireLease",
            "parameters": { "priority": "Batch", "memory_bytes": 1024 * 1024 * 1024 }
        });
        let mut b = serde_json::to_vec(&req).unwrap();
        b.push(0);
        stream.write_all(&b).await.unwrap();

        let mut buf_reader = BufReader::new(&mut stream);
        let mut resp_buf = Vec::new();
        buf_reader.read_until(0, &mut resp_buf).await.unwrap();
        batch_clients.push(stream);
    }

    // Preempt all 10 batch leases with an interactive lease
    let giant_lease = arbiter
        .acquire_lease(LeasePriority::Interactive, 30 * 1024 * 1024 * 1024, Some("plane-rogue-gpu".into()), None, None)
        .await
        .expect("Preempt batch leases");

    // All 10 batch clients drop sockets while in Preempted state
    for stream in batch_clients {
        drop(stream);
    }

    // Release giant interactive lease
    arbiter.release_lease(giant_lease.id).await.unwrap();

    // Poll until all leases are Expired
    for _ in 0..50 {
        let leases = arbiter.list_leases().await;
        if !leases.is_empty() && leases.iter().all(|l| l.state == LeaseState::Expired) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // Invariant 1: Zero zombie leases
    let final_leases = arbiter.list_leases().await;
    assert!(!final_leases.is_empty(), "Must have recorded leases");
    assert!(
        final_leases.iter().all(|l| l.state == LeaseState::Expired),
        "All leases must be in Expired state (zero zombies)"
    );

    // Invariant 2: 100% memory recovery
    let topo = arbiter.get_topology().await;
    assert_eq!(
        topo.planes[0].available_memory_bytes, total_mem,
        "All memory must be 100% restored after rogue disconnects"
    );

    // Invariant 3: Idempotent release
    for l in final_leases {
        let _ = arbiter.release_lease(l.id).await;
    }
    let topo_after = arbiter.get_topology().await;
    assert_eq!(
        topo_after.planes[0].available_memory_bytes, total_mem,
        "Idempotent release must never double-credit memory"
    );

    srv_handle.abort();
}
