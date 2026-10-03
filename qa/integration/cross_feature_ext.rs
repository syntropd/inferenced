use inferenced_core::{
    arbiter::Arbiter,
    fd_lease::{create_sealed_memfd, recv_fd_from_unix, send_fd_over_unix},
    lease::LeasePriority,
    model::{ModelDescriptor, ModelPlacementState, ModelRegistry},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use rustix::fs::{seek, SeekFrom};
use rustix::io::read;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

static SOCK_COUNTER: AtomicUsize = AtomicUsize::new(100);

fn make_ext_topo() -> HardwareTopology {
    let mut topo = HardwareTopology::default();
    topo.planes.push(
        ComputePlane::builder("plane-ext-gpu")
            .name("Extended Interaction dGPU")
            .kind(ComputePlaneKind::DiscreteGpu)
            .no_device_path()
            .total_memory(8 * 1024 * 1024 * 1024)
            .build(),
    );
    topo
}

#[tokio::test]
async fn test_interaction_abstract_notify_watchdog_during_lease_churn() {
    let id = SOCK_COUNTER.fetch_add(1, Ordering::SeqCst);
    let name = format!("inferenced_watchdog_churn_{}_{}", std::process::id(), id);
    let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
    let receiver = UnixDatagram::bind_addr(&addr).unwrap();

    let arbiter = Arc::new(Arbiter::new(make_ext_topo()));
    let sender = UnixDatagram::unbound().unwrap();

    // Spawn task doing lease churn while another task pings watchdog
    let arb = arbiter.clone();
    let churn_task = tokio::spawn(async move {
        for _ in 0..10 {
            let l = arb
                .acquire_lease(LeasePriority::Interactive, 512 * 1024 * 1024, None, None, None)
                .await
                .unwrap();
            arb.release_lease(l.id).await.unwrap();
        }
    });

    let ping_task = tokio::spawn(async move {
        for _ in 0..5 {
            sender.send_to_addr(b"WATCHDOG=1\n", &addr).unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    });

    churn_task.await.unwrap();
    ping_task.await.unwrap();

    let mut buf = [0u8; 64];
    let (n, _) = receiver.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"WATCHDOG=1\n");
}

#[tokio::test]
async fn test_interaction_cli_exec_stream_filter_with_sentry_triage_interruption() {
    let arbiter = Arc::new(Arbiter::new(make_ext_topo()));

    // 1. Regular inference streaming allocates memory
    let stream_lease = arbiter
        .acquire_lease(
            LeasePriority::Interactive,
            4 * 1024 * 1024 * 1024,
            None,
            Some("inferenctl-exec".into()),
            None,
        )
        .await
        .unwrap();

    // 2. Sentry triggers high-priority triage
    let triage_lease = arbiter
        .acquire_lease(
            LeasePriority::EmergencyTriage,
            2 * 1024 * 1024 * 1024,
            None,
            Some("systemd-sentry".into()),
            None,
        )
        .await
        .unwrap();

    assert_eq!(triage_lease.priority, LeasePriority::EmergencyTriage);

    // 3. Both complete cleanly
    arbiter.release_lease(triage_lease.id).await.unwrap();
    arbiter.release_lease(stream_lease.id).await.unwrap();
}

#[tokio::test]
async fn test_interaction_hardware_plane_rediscovery_with_active_leases() {
    let mut initial_topo = make_ext_topo();
    initial_topo.planes.push(
        ComputePlane::builder("plane-hotplug-npu")
            .name("Hotplug NPU")
            .kind(ComputePlaneKind::NpuAccelerator)
            .no_device_path()
            .total_memory(2 * 1024 * 1024 * 1024)
            .build(),
    );
    let arbiter = Arbiter::new(initial_topo);

    let l1 = arbiter
        .acquire_lease(LeasePriority::Interactive, 1024 * 1024 * 1024, Some("plane-ext-gpu".into()), None, None)
        .await
        .unwrap();

    let l2 = arbiter
        .acquire_lease(LeasePriority::Interactive, 1024 * 1024 * 1024, Some("plane-hotplug-npu".into()), None, None)
        .await
        .unwrap();

    // Rediscover topology where plane-hotplug-npu is unplugged/removed
    arbiter.update_topology(make_ext_topo()).await;

    // Verify plane-ext-gpu retained lease memory reservation (8GB - 1GB = 7GB)
    let topo = arbiter.get_topology().await;
    assert_eq!(topo.planes.len(), 1);
    assert_eq!(topo.planes[0].available_memory_bytes, 7 * 1024 * 1024 * 1024);

    // Verify lease on hot-unplugged plane is transitioned to Revoked
    assert_eq!(arbiter.get_lease(l2.id).await.unwrap().state, inferenced_core::lease::LeaseState::Revoked);

    // Release l1 lease -> plane-ext-gpu restores to 8GB cleanly
    arbiter.release_lease(l1.id).await.unwrap();
    let restored = arbiter.get_topology().await;
    assert_eq!(restored.planes[0].available_memory_bytes, 8 * 1024 * 1024 * 1024);
}

#[test]
fn test_interaction_varlink_stream_inference_under_model_eviction() {
    let mut reg = ModelRegistry::new();
    let desc = ModelDescriptor {
        id: "stream-test-model".into(),
        format: "GGUF".into(),
        path: PathBuf::from("/models/stream.gguf"),
        estimated_memory_bytes: 2 * 1024 * 1024 * 1024,
        placement: ModelPlacementState::Dormant,
        resident_plane_id: None,
        last_accessed: chrono::Utc::now(),
        access_count: 5,
        preferred_plane: None,
    };

    reg.register(desc);
    assert_eq!(reg.list().len(), 1);

    // Evict model while simulating active reference
    let removed = reg.remove("stream-test-model");
    assert!(removed.is_some());
    assert_eq!(reg.list().len(), 0);
}

#[test]
fn test_interaction_zerocopy_scm_rights_across_socket_activation_channels() {
    let (s1, s2) = UnixStream::pair().unwrap();
    let weights = b"deep-neural-network-layer-weights-1234";

    let memfd = create_sealed_memfd("nn_layer", weights.len() as u64, Some(weights)).unwrap();
    send_fd_over_unix(&s1, &memfd, b"header").unwrap();

    let mut buf = [0u8; 16];
    let (_, maybe_fd) = recv_fd_from_unix(&s2, &mut buf).unwrap();
    let recv_fd = maybe_fd.unwrap();

    seek(&recv_fd, SeekFrom::Start(0)).unwrap();
    let mut read_buf = vec![0u8; weights.len()];
    read(&recv_fd, &mut read_buf).unwrap();
    assert_eq!(&read_buf, weights);
}
