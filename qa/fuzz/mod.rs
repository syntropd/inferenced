use inferenced_core::{
    arbiter::Arbiter,
    lease::LeasePriority,
    topology::{cpu, ComputePlane, ComputePlaneKind, HardwareTopology},
};
use std::sync::Arc;

mod stream_fuzz;
mod varlink_fuzz;

#[test]
fn test_fuzz_parse_kb_with_random_garbage() {
    let fuzzed_inputs = vec![
        "99999999999999999999999999999 kB", // huge overflow
        "-12345 kB",
        "0.0001 kB",
        "NaN kB",
        "\0\0\0\0 kB",
        "1024 \t\r\n MB",
        "   \x1b[31m1024\x1b[0m kB   ",
        "1 2 3 4 5 kB",
        "!@#$%^&*()_+ kB",
        "中文 1024 kB",
        "1024kB",
        "1024",
    ];

    for input in fuzzed_inputs {
        // Must never panic!
        let _ = cpu::parse_kb(input);
    }
}

#[tokio::test]
async fn test_fuzz_concurrent_randomized_leases() {
    let mut topo = HardwareTopology::default();
    topo.planes.push(ComputePlane {
        id: "plane-fuzz".into(),
        name: "Fuzzing Stress Plane".into(),
        kind: ComputePlaneKind::DiscreteGpu,
        device_path: None,
        total_memory_bytes: 100 * 1024 * 1024 * 1024,     // 100GB
        available_memory_bytes: 100 * 1024 * 1024 * 1024,
        numa_node: None,
        supported_formats: vec![],
        is_triage_reserved: false,
        is_quarantined: false,
        hardware_features: vec![],
        p2p_links: None,
        kernel_used_memory: 0,
    });

    let arbiter = Arc::new(Arbiter::new(topo));
    let mut handles = Vec::new();

    // Spawn 20 concurrent tasks acquiring and releasing random leases
    for i in 0..20 {
        let arb = arbiter.clone();
        handles.push(tokio::spawn(async move {
            let priority = if i % 5 == 0 {
                LeasePriority::EmergencyTriage
            } else if i % 2 == 0 {
                LeasePriority::Interactive
            } else {
                LeasePriority::Batch
            };

            let needed = (i as u64 + 1) * 256 * 1024 * 1024; // between 256MB and 5GB
            if let Ok(lease) = arb.acquire_lease(priority, needed, None, None, None).await {
                // Yield thread
                tokio::task::yield_now().await;
                let _ = arb.release_lease(lease.id).await;
            }
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    // Verify all memory is properly reclaimed
    let final_topo = arbiter.get_topology().await;
    assert_eq!(
        final_topo.planes[0].available_memory_bytes,
        100 * 1024 * 1024 * 1024
    );
}
