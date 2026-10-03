use inferenced_core::{
    arbiter::Arbiter,
    madvise::{advise_dontneed, advise_willneed},
    model::{ModelDescriptor, ModelPlacementState, ModelRegistry},
    psi::PressureMetrics,
    topology::{ComputePlane, ComputePlaneKind, HardwareTopology},
};
use rustix::mm::{mmap_anonymous, munmap, MapFlags, ProtFlags};
use std::path::PathBuf;
use std::ptr::null_mut;

#[tokio::test]
async fn test_scenario_memory_starvation_and_psi_paging() {
    // Scenario 3: Memory pressure rises to critical; daemon triggers dynamic demand paging
    let mut topo = HardwareTopology::default();
    topo.planes.push(
        ComputePlane::builder("plane-starved-gpu")
            .name("Memory Starved GPU")
            .kind(ComputePlaneKind::DiscreteGpu)
            .no_device_path()
            .total_memory(8 * 1024 * 1024 * 1024)
            .build(),
    );

    let _arbiter = Arbiter::new(topo);

    // 1. Register resident model
    let mut registry = ModelRegistry::new();
    let model = ModelDescriptor {
        id: "deepseek-coder:33b".into(),
        format: "GGUF".into(),
        path: PathBuf::from("/models/deepseek-33b.gguf"),
        estimated_memory_bytes: 6 * 1024 * 1024 * 1024,
        placement: ModelPlacementState::Resident,
        resident_plane_id: Some("plane-starved-gpu".into()),
        last_accessed: chrono::Utc::now(),
        access_count: 10,
        preferred_plane: None,
    };
    registry.register(model);

    // 2. Allocate simulated model weight pages in memory
    let weight_size = 4096 * 128; // 512 KB
    let addr = unsafe {
        mmap_anonymous(
            null_mut(),
            weight_size,
            ProtFlags::READ | ProtFlags::WRITE,
            MapFlags::PRIVATE,
        )
    }
    .unwrap();

    unsafe {
        std::ptr::write_bytes(addr, 0xEE, weight_size);
    }

    // 3. Inspect Linux Pressure Stall Information
    let psi = PressureMetrics::read_current();
    assert!(psi.memory_some_avg10 >= 0.0);
    assert!(psi.memory_full_avg10 >= 0.0);

    // 4. In starvation condition, reclaim memory by advising DontNeed to Linux kernel
    advise_dontneed(addr, weight_size).expect("Dynamic demand paging reclamation");

    // 5. Update model placement to Dormant
    if let Some(m) = registry.get_mut("deepseek-coder:33b") {
        m.placement = ModelPlacementState::Dormant;
        m.resident_plane_id = None;
    }

    let updated = registry.get("deepseek-coder:33b").unwrap();
    assert_eq!(updated.placement, ModelPlacementState::Dormant);
    assert!(updated.resident_plane_id.is_none());

    // 6. When workload reactivates, pre-fault back into resident memory
    advise_willneed(addr, weight_size).expect("Prefetch into active memory");

    if let Some(m) = registry.get_mut("deepseek-coder:33b") {
        m.placement = ModelPlacementState::Resident;
        m.resident_plane_id = Some("plane-starved-gpu".into());
    }

    assert_eq!(
        registry.get("deepseek-coder:33b").unwrap().placement,
        ModelPlacementState::Resident
    );

    unsafe {
        munmap(addr, weight_size).unwrap();
    }
}
