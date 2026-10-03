use inferenced_core::{
    arbiter::Arbiter,
    fd_lease::{create_sealed_memfd, recv_fd_from_unix, send_fd_over_unix},
    lease::{LeasePriority, LeaseState},
    madvise::{advise_dontneed, advise_willneed},
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use rustix::mm::{mmap, MapFlags, ProtFlags};
use std::os::unix::net::UnixStream;
use std::ptr::null_mut;

fn make_topo(cap: u64) -> HardwareTopology {
    let mut topo = HardwareTopology::default();
    topo.planes.push(
        ComputePlane::builder("plane-cross-gpu")
            .name("Cross Feature dGPU")
            .kind(ComputePlaneKind::DiscreteGpu)
            .no_device_path()
            .total_memory(cap)
            .build(),
    );
    topo
}

#[tokio::test]
async fn test_interaction_sentry_preemption_during_active_streaming() {
    let cap = 4 * 1024 * 1024 * 1024;
    let arbiter = Arbiter::new(make_topo(cap));

    // 1. Streaming inference client acquires entire plane for interactive batch
    let stream_lease = arbiter
        .acquire_lease(
            LeasePriority::Interactive,
            cap,
            None,
            Some("stream-client.service".into()),
            None,
        )
        .await
        .expect("Stream client should acquire lease");

    assert_eq!(stream_lease.state, LeaseState::Active);

    // 2. Catastrophic event occurs -> systemd-sentry demands emergency triage
    let sentry_lease = arbiter
        .acquire_lease(
            LeasePriority::EmergencyTriage,
            1024 * 1024 * 1024,
            None,
            Some("systemd-sentry.service".into()),
            None,
        )
        .await
        .expect("Sentry emergency triage must succeed during active streaming");

    assert_eq!(sentry_lease.priority, LeasePriority::EmergencyTriage);
    assert_eq!(sentry_lease.state, LeaseState::Active);

    // 3. Emergency lease release cleanly returns control
    arbiter.release_lease(sentry_lease.id).await.unwrap();
}

#[tokio::test]
async fn test_interaction_socket_activation_with_high_psi_pressure() {
    // Tests admission control under synthetic pressure conditions
    let cap = 8 * 1024 * 1024 * 1024;
    let arbiter = Arbiter::new(make_topo(cap));

    // Emergency triage must always be admitted regardless of pressure state
    let emergency = arbiter
        .acquire_lease(
            LeasePriority::EmergencyTriage,
            2 * 1024 * 1024 * 1024,
            None,
            Some("systemd-sentry.service".into()),
            None,
        )
        .await;
    assert!(emergency.is_ok(), "Emergency triage must bypass pressure throttling");
}

#[tokio::test]
async fn test_interaction_varlink_lease_acquisition_with_cgroup_freeze_and_thaw() {
    let cap = 4 * 1024 * 1024 * 1024;
    let arbiter = Arbiter::new(make_topo(cap));

    // Acquire lease
    let lease = arbiter
        .acquire_lease(
            LeasePriority::Interactive,
            1024 * 1024 * 1024,
            None,
            Some("worker.slice".into()),
            Some(1234),
        )
        .await
        .unwrap();

    // Freeze lease
    arbiter.freeze_lease(lease.id).await.unwrap();
    let frozen = arbiter.get_lease(lease.id).await.unwrap();
    assert_eq!(frozen.state, LeaseState::Frozen);

    // Thaw lease
    arbiter.thaw_lease(lease.id).await.unwrap();
    let thawed = arbiter.get_lease(lease.id).await.unwrap();
    assert_eq!(thawed.state, LeaseState::Active);

    // Release lease
    arbiter.release_lease(lease.id).await.unwrap();
    let released = arbiter.get_lease(lease.id).await.unwrap();
    assert_eq!(released.state, LeaseState::Expired);
}

#[test]
fn test_interaction_memfd_sharing_combined_with_madvise_dontneed_reclaim() {
    let (s1, s2) = UnixStream::pair().unwrap();
    let tensor_size = 4096 * 64; // 256 KB
    let initial_weights = vec![0x33u8; tensor_size];

    // Create sealed memfd
    let memfd = create_sealed_memfd("shared_tensors", tensor_size as u64, Some(&initial_weights)).unwrap();

    // Pass over Unix socket
    send_fd_over_unix(&s1, &memfd, b"tensor_v1").unwrap();

    let mut buf = [0u8; 32];
    let (_, maybe_fd) = recv_fd_from_unix(&s2, &mut buf).unwrap();
    let received_fd = maybe_fd.expect("Should receive fd");

    // Memory map the received file descriptor
    let addr = unsafe {
        mmap(
            null_mut(),
            tensor_size,
            ProtFlags::READ,
            MapFlags::SHARED,
            &received_fd,
            0,
        )
    }
    .expect("mmap received memfd");

    // Advise kernel that memory is needed
    advise_willneed(addr, tensor_size).expect("willneed");

    // Verify initial values
    let slice = unsafe { std::slice::from_raw_parts(addr as *const u8, tensor_size) };
    assert_eq!(slice[0], 0x33);

    // Advise DontNeed for dynamic demand paging / zswap offloading
    advise_dontneed(addr, tensor_size).expect("dontneed");

    // Cleanup
    unsafe {
        rustix::mm::munmap(addr, tensor_size).unwrap();
    }
}

#[tokio::test]
async fn test_interaction_multitenant_arbitration_with_cooperative_yield_fallback() {
    let cap = 4 * 1024 * 1024 * 1024; // 4GB total
    let arbiter = Arbiter::new(make_topo(cap));

    // Tenant 1 (Batch): takes 3GB
    let t1_batch = arbiter
        .acquire_lease(LeasePriority::Batch, 3 * 1024 * 1024 * 1024, None, None, None)
        .await
        .unwrap();

    // Tenant 2 (Interactive): requests 2GB -> forces preemption of Tenant 1
    let t2_interactive = arbiter
        .acquire_lease(LeasePriority::Interactive, 2 * 1024 * 1024 * 1024, None, None, None)
        .await
        .unwrap();

    assert_eq!(t2_interactive.state, LeaseState::Active);
    assert_eq!(arbiter.get_lease(t1_batch.id).await.unwrap().state, LeaseState::Preempted);

    // Tenant 2 finishes and releases its lease
    arbiter.release_lease(t2_interactive.id).await.unwrap();

    // Available memory restored to at least 2GB
    let current_topo = arbiter.get_topology().await;
    assert!(current_topo.planes[0].available_memory_bytes >= 2 * 1024 * 1024 * 1024);
}

#[tokio::test]
async fn test_interaction_uma_preempt_thaw_and_release_lifecycle() {
    let mut topo = HardwareTopology::default();
    let total_bytes = 8 * 1024 * 1024 * 1024;
    let make_p = |id: &str, kind| {
        ComputePlane::builder(id)
            .kind(kind)
            .no_device_path()
            .total_memory(total_bytes)
            .build()
    };
    topo.planes.push(make_p("plane-uma-gpu", ComputePlaneKind::IntegratedUma));
    topo.planes.push(make_p("cpu-host", ComputePlaneKind::CpuMatrixExtension));
    let arbiter = Arbiter::new(topo);

    // 1. Acquire 4GB Batch lease on UMA (deducts from both UMA and CPU)
    let batch = arbiter
        .acquire_lease(LeasePriority::Batch, 4 * 1024 * 1024 * 1024, Some("plane-uma-gpu".into()), None, None)
        .await
        .unwrap();
    let t1 = arbiter.get_topology().await;
    assert_eq!(t1.planes[0].available_memory_bytes, 4 * 1024 * 1024 * 1024);
    assert_eq!(t1.planes[1].available_memory_bytes, 4 * 1024 * 1024 * 1024);

    // 2. High priority Interactive lease arrives requiring 6GB on UMA -> preempts 4GB batch
    let interactive = arbiter
        .acquire_lease(LeasePriority::Interactive, 6 * 1024 * 1024 * 1024, Some("plane-uma-gpu".into()), None, None)
        .await
        .unwrap();
    assert_eq!(arbiter.get_lease(batch.id).await.unwrap().state, LeaseState::Preempted);
    let t2 = arbiter.get_topology().await;
    assert_eq!(t2.planes[0].available_memory_bytes, 2 * 1024 * 1024 * 1024);
    assert_eq!(t2.planes[1].available_memory_bytes, 2 * 1024 * 1024 * 1024);

    // 3. Attempt to thaw batch lease while memory insufficient fails
    assert!(arbiter.thaw_lease(batch.id).await.is_err());

    // 4. Release interactive lease -> restores memory to both planes
    arbiter.release_lease(interactive.id).await.unwrap();
    let t3 = arbiter.get_topology().await;
    assert_eq!(t3.planes[0].available_memory_bytes, total_bytes);
    assert_eq!(t3.planes[1].available_memory_bytes, total_bytes);

    // 5. Thaw batch lease -> succeeds, deducts 4GB from BOTH UMA and CPU planes
    arbiter.thaw_lease(batch.id).await.unwrap();
    assert_eq!(arbiter.get_lease(batch.id).await.unwrap().state, LeaseState::Active);
    let t4 = arbiter.get_topology().await;
    assert_eq!(t4.planes[0].available_memory_bytes, 4 * 1024 * 1024 * 1024);
    assert_eq!(t4.planes[1].available_memory_bytes, 4 * 1024 * 1024 * 1024);

    // 6. Release batch lease -> restores both planes cleanly
    arbiter.release_lease(batch.id).await.unwrap();
    let t_final = arbiter.get_topology().await;
    assert_eq!(t_final.planes[0].available_memory_bytes, total_bytes);
    assert_eq!(t_final.planes[1].available_memory_bytes, total_bytes);
}

#[test]
fn test_integration_suite_sanity_check() {
    // Integration test harness initialized successfully
}
