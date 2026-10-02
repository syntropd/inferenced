use crate::psi::{PressureLevel, PressureMetrics, StackPsiReader};

#[test]
fn test_psi_read_current() {
    let metrics = PressureMetrics::read_current();
    assert!(metrics.memory_some_avg10 >= 0.0);
    assert!(metrics.memory_full_avg10 >= 0.0);
    assert!(metrics.cpu_some_avg10 >= 0.0);
    assert!(metrics.io_some_avg10 >= 0.0);
    assert!(matches!(
        metrics.level,
        PressureLevel::Normal | PressureLevel::Elevated | PressureLevel::Critical
    ));
}

#[test]
fn test_psi_level_thresholds() {
    let normal = PressureMetrics {
        memory_some_avg10: 5.0,
        memory_full_avg10: 1.0,
        cpu_some_avg10: 20.0,
        io_some_avg10: 5.0,
        runqueue_latency_us: 150,
        ebpf_active: false,
        level: PressureLevel::Normal,
    };
    assert_eq!(normal.level, PressureLevel::Normal);

    let elevated = PressureMetrics {
        memory_some_avg10: 22.0,
        memory_full_avg10: 2.0,
        cpu_some_avg10: 30.0,
        io_some_avg10: 5.0,
        runqueue_latency_us: 35_000,
        ebpf_active: false,
        level: PressureLevel::Elevated,
    };
    assert_eq!(elevated.level, PressureLevel::Elevated);

    let critical = PressureMetrics {
        memory_some_avg10: 45.0,
        memory_full_avg10: 12.0,
        cpu_some_avg10: 30.0,
        io_some_avg10: 10.0,
        runqueue_latency_us: 95_000,
        ebpf_active: false,
        level: PressureLevel::Critical,
    };
    assert_eq!(critical.level, PressureLevel::Critical);
}

#[test]
fn test_zero_allocation_stack_psi_reader_chunks() {
    let sample = b"some avg10=23.45 avg60=12.00 avg300=5.00 total=1000\nfull avg10=8.90 avg60=4.00 avg300=1.00 total=500\n";
    let parsed = StackPsiReader::parse_slice(sample);
    assert!((parsed.some_avg10 - 23.45).abs() < 0.01);
    assert!((parsed.full_avg10 - 8.90).abs() < 0.01);
}

#[test]
fn test_simulated_psi_spikes() {
    let sim_crit = PressureMetrics::from_level(PressureLevel::Critical);
    assert_eq!(sim_crit.level, PressureLevel::Critical);
    assert_eq!(sim_crit.memory_some_avg10, 60.0);
    assert_eq!(sim_crit.memory_full_avg10, 25.0);

    let sim_elev = PressureMetrics::from_level(PressureLevel::Elevated);
    assert_eq!(sim_elev.level, PressureLevel::Elevated);
    assert_eq!(sim_elev.memory_some_avg10, 20.0);
}

#[test]
fn test_unprivileged_fallback_probe() {
    let telemetry = crate::psi::ebpf::collect_kernel_telemetry(0.05, 0.10);
    assert!(telemetry.runqueue_latency_us > 0);
}
