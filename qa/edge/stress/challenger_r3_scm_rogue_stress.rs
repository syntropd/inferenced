use inferenced_core::{
    arbiter::Arbiter,
    fd_lease::{create_sealed_memfd, recv_fd_from_unix, send_fd_over_unix},
    lease::{LeaseId, LeasePriority, LeaseState},
    madvise::{advise_dontneed, advise_willneed},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use rustix::fs::{fcntl_get_seals, ftruncate, SealFlags};
use rustix::io::write;
use rustix::mm::{mmap, munmap, MapFlags, ProtFlags};
use serde_json::{json, Value};
use std::os::unix::net::{UnixListener as StdListener, UnixStream as StdStream};
use std::ptr;
use std::sync::{Arc, Barrier, Mutex};
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

static SCM_CHALLENGE_LOCK: Mutex<()> = Mutex::new(());

fn count_open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0)
}

#[test]
fn test_adversarial_scm_rights_50_client_fanout_and_fd_leaks() {
    let _lock = SCM_CHALLENGE_LOCK.lock().unwrap();
    let baseline_fds = count_open_fds();
    let size = 8 * 1024 * 1024; // 8MB
    let mut payload = vec![0u8; size];
    for (i, b) in payload.iter_mut().enumerate() {
        *b = ((i * 17 + 123) & 0xFF) as u8;
    }

    let memfd = create_sealed_memfd("adv_sealed_model_8mb", size as u64, Some(&payload))
        .expect("create_sealed_memfd");
    let seals = fcntl_get_seals(&memfd).expect("fcntl_get_seals");
    assert!(seals.contains(SealFlags::SEAL | SealFlags::GROW | SealFlags::SHRINK | SealFlags::WRITE));

    let dir = tempdir().expect("tempdir");
    let sock_path = dir.path().join("scm_adv_fanout.sock");
    let listener = StdListener::bind(&sock_path).expect("bind listener");

    let server_memfd = memfd;
    let server_handle = std::thread::spawn(move || {
        for _ in 0..50 {
            let (stream, _) = listener.accept().expect("accept");
            send_fd_over_unix(&stream, &server_memfd, b"CHALLENGER_TENSOR_V1").expect("send_fd");
        }
        server_memfd
    });

    let barrier = Arc::new(Barrier::new(50));
    let mut client_handles = Vec::new();
    for client_id in 0..50 {
        let path = sock_path.clone();
        let bar = barrier.clone();
        let expected = payload.clone();
        client_handles.push(std::thread::spawn(move || {
            bar.wait();
            let stream = StdStream::connect(&path).expect("connect");
            let mut buf = [0u8; 64];
            let (n, fd_opt) = recv_fd_from_unix(&stream, &mut buf).expect("recv_fd");
            assert_eq!(&buf[..n], b"CHALLENGER_TENSOR_V1");
            let recvd_fd = fd_opt.expect("received fd");

            // Direct write mutation must fail with EPERM
            let w_err = write(&recvd_fd, b"CORRUPT").expect_err("write must fail");
            assert_eq!(w_err, rustix::io::Errno::PERM);

            // ftruncate mutation must fail with EPERM
            let t_err = ftruncate(&recvd_fd, 4096).expect_err("ftruncate must fail");
            assert_eq!(t_err, rustix::io::Errno::PERM);

            // Shared writable mmap must fail with EPERM due to F_SEAL_WRITE
            let mmap_write_res = unsafe {
                mmap(ptr::null_mut(), size, ProtFlags::READ | ProtFlags::WRITE, MapFlags::SHARED, &recvd_fd, 0)
            };
            assert!(mmap_write_res.is_err(), "Shared writable mmap on sealed memfd must fail");

            // Read-only mmap and data integrity verification
            let ptr = unsafe {
                mmap(ptr::null_mut(), size, ProtFlags::READ, MapFlags::SHARED, &recvd_fd, 0).expect("mmap read")
            };
            let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, size) };
            assert_eq!(slice[0], expected[0], "Mismatch at offset 0 (client {})", client_id);
            assert_eq!(slice[size / 2], expected[size / 2], "Mismatch at mid (client {})", client_id);
            assert_eq!(slice[size - 1], expected[size - 1], "Mismatch at end (client {})", client_id);

            assert!(advise_willneed(ptr, size).is_ok());
            assert!(advise_dontneed(ptr, size).is_ok());
            unsafe { munmap(ptr, size).expect("munmap"); }
            drop(recvd_fd);
            drop(stream);
        }));
    }

    for h in client_handles { h.join().expect("client join"); }
    let returned = server_handle.join().expect("server join");
    drop(returned);
    drop(dir);

    let final_fds = count_open_fds();
    assert!(final_fds <= baseline_fds, "FD leak detected! Baseline: {}, Final: {}", baseline_fds, final_fds);
}

fn create_rogue_plane(total_mem: u64) -> HardwareTopology {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-rogue-adv".into(), name: "Adversarial Rogue Plane".into(),
        kind: ComputePlaneKind::DiscreteGpu, device_path: None,
        total_memory_bytes: total_mem, available_memory_bytes: total_mem,
        numa_node: None, supported_formats: vec![],
        is_triage_reserved: false, is_quarantined: false, hardware_features: vec![],
    });
    topo
}


async fn run_mock_varlink_server(listener: UnixListener, arbiter: Arc<Arbiter>) {
    loop {
        let (stream, _) = match listener.accept().await { Ok(s) => s, Err(_) => break };
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
                        let req = match parsed { Ok(v) => v, Err(_) => break };
                        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
                        let params = req.get("parameters");

                        match method {
                            "io.systemd.inferenced1.AcquireLease" => {
                                let prio = match params.and_then(|p| p.get("priority")).and_then(|v| v.as_str()).unwrap_or("Interactive") {
                                    "Batch" => LeasePriority::Batch,
                                    "EmergencyTriage" => LeasePriority::EmergencyTriage,
                                    _ => LeasePriority::Interactive,
                                };
                                let mem = params.and_then(|p| p.get("memory_bytes")).and_then(|v| v.as_u64()).unwrap_or(1024 * 1024 * 1024);
                                match arb.acquire_lease(prio, mem, Some("plane-rogue-adv".into()), None, None).await {
                                    Ok(l) => {
                                        active_leases.push(l.id);
                                        let rep = json!({ "parameters": { "lease_id": l.id.to_string() } });
                                        let mut b = serde_json::to_vec(&rep).unwrap();
                                        b.push(0);
                                        if writer.write_all(&b).await.is_err() { break; }
                                    }
                                    Err(_) => break,
                                }
                            }
                            "io.systemd.inferenced1.StreamInference" => {
                                for i in 0..5 {
                                    let chunk = json!({ "parameters": { "chunk": format!("tok_{}", i) }, "continues": i < 4 });
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
            for lid in active_leases { let _ = arb.release_lease(lid).await; }
        });
    }
}

#[tokio::test]
async fn test_adversarial_rogue_client_disconnect_storm_varlink() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("rogue_adv.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let total_mem = 32 * 1024 * 1024 * 1024; // 32GB
    let arbiter = Arc::new(Arbiter::new(create_rogue_plane(total_mem)));

    let srv_arb = arbiter.clone();
    let srv_handle = tokio::spawn(async move { run_mock_varlink_server(listener, srv_arb).await; });

    // Wave 1: 10 clients acquire lease, start inference, and drop mid-stream
    for i in 0..10 {
        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let req = json!({ "method": "io.systemd.inferenced1.AcquireLease", "parameters": { "priority": "Interactive", "memory_bytes": 1024 * 1024 * 1024 } });
        let mut b = serde_json::to_vec(&req).unwrap();
        b.push(0);
        stream.write_all(&b).await.unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut resp_buf = Vec::new();
        reader.read_until(0, &mut resp_buf).await.unwrap();

        let sreq = json!({ "method": "io.systemd.inferenced1.StreamInference", "parameters": { "prompt": format!("p_{}", i) } });
        let mut sb = serde_json::to_vec(&sreq).unwrap();
        sb.push(0);
        let _ = stream.write_all(&sb).await;
        drop(stream);
    }

    // Wave 2: 10 clients send malformed JSON
    for _ in 0..10 {
        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let _ = stream.write_all(b"{\"bad-json-frame...").await;
        drop(stream);
    }

    // Wave 3: 10 batch clients get preempted, then drop socket while Preempted
    let mut batch_streams = Vec::new();
    for _ in 0..10 {
        let mut stream = UnixStream::connect(&sock).await.unwrap();
        let req = json!({ "method": "io.systemd.inferenced1.AcquireLease", "parameters": { "priority": "Batch", "memory_bytes": 1024 * 1024 * 1024 } });
        let mut b = serde_json::to_vec(&req).unwrap();
        b.push(0);
        stream.write_all(&b).await.unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut resp_buf = Vec::new();
        reader.read_until(0, &mut resp_buf).await.unwrap();
        batch_streams.push(stream);
    }

    let giant = arbiter.acquire_lease(LeasePriority::Interactive, 30 * 1024 * 1024 * 1024, Some("plane-rogue-adv".into()), None, None).await.unwrap();
    for s in batch_streams { drop(s); }
    arbiter.release_lease(giant.id).await.unwrap();

    // Verify all leases expired and 100% memory restored
    for _ in 0..50 {
        let leases = arbiter.list_leases().await;
        if !leases.is_empty() && leases.iter().all(|l| l.state == LeaseState::Expired) { break; }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let final_leases = arbiter.list_leases().await;
    assert!(final_leases.iter().all(|l| l.state == LeaseState::Expired), "Zombie leases detected!");
    assert_eq!(arbiter.get_topology().await.planes[0].available_memory_bytes, total_mem, "Memory leak detected!");

    // Idempotent release check
    for l in final_leases { let _ = arbiter.release_lease(l.id).await; }
    assert_eq!(arbiter.get_topology().await.planes[0].available_memory_bytes, total_mem, "Double credit bug!");

    srv_handle.abort();
}

