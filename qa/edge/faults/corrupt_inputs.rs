use inferenced_core::topology::cpu;
use std::io::Write;
use tempfile::NamedTempFile;

#[test]
fn test_edge_corrupt_meminfo_garbage_lines() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "!@#$%^&*()_+").unwrap();
    writeln!(file, "MemTotal: not_a_number kB").unwrap();
    writeln!(file, "MemAvailable: null kB").unwrap();

    let (total, avail) = cpu::read_meminfo_from(file.path().to_str().unwrap());
    assert_eq!(total, 0);
    assert_eq!(avail, 0);
}

#[test]
fn test_edge_corrupt_meminfo_missing_avail() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "MemTotal:       8192 kB").unwrap();
    // Missing MemAvailable

    let (total, avail) = cpu::read_meminfo_from(file.path().to_str().unwrap());
    assert_eq!(total, 8192 * 1024);
    assert_eq!(avail, 0);
}

#[test]
fn test_edge_corrupt_cpuinfo_no_flags() {
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "processor : 0").unwrap();
    writeln!(file, "model name : Unknown CPU").unwrap();

    let (amx, vnni, avx2) = cpu::detect_cpu_features(file.path().to_str().unwrap());
    assert!(!amx);
    assert!(!vnni);
    assert!(!avx2);
}

#[test]
fn test_edge_corrupt_cpuinfo_zero_bytes() {
    let file = NamedTempFile::new().unwrap();
    let cores = cpu::count_cpu_cores(file.path().to_str().unwrap());
    assert_eq!(cores, 1, "Must default to at least 1 core for safe scheduling");
}

#[test]
fn test_edge_corrupt_toml_config_recovery() {
    let invalid_toml = "key = [unclosed array\n[another_section";
    let res: Result<toml::Value, _> = toml::from_str(invalid_toml);
    assert!(res.is_err(), "Invalid TOML must cleanly return parse error");
}

#[test]
fn test_edge_empty_toml_config_parses_empty_table() {
    let empty_toml = "";
    let res: Result<toml::Value, _> = toml::from_str(empty_toml);
    assert!(res.is_ok());
    let val = res.unwrap();
    assert!(val.as_table().unwrap().is_empty());
}
