use std::path::PathBuf;
use std::process::Command;

fn find_inferenctl() -> PathBuf {
    if let Ok(cargo_bin) = std::env::var("CARGO_BIN_EXE_inferenctl") {
        return PathBuf::from(cargo_bin);
    }
    let mut path = std::env::current_exe().unwrap_or_default();
    while path.pop() {
        if path.file_name().map(|n| n == "target").unwrap_or(false) {
            let candidate = path.join("debug/inferenctl");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("target/debug/inferenctl")
}

#[test]
fn test_edge_cli_unknown_subcommand_exit_code() {
    let bin = find_inferenctl();
    let out = Command::new(&bin).arg("unknown-subcommand-xyz").output().unwrap();
    assert!(!out.status.success(), "Unknown subcommand must exit with non-zero status");
}

#[test]
fn test_edge_cli_exec_missing_model_arg() {
    let bin = find_inferenctl();
    let out = Command::new(&bin).arg("exec").output().unwrap();
    assert!(!out.status.success(), "Missing model argument must fail");
}

#[test]
fn test_edge_cli_inspect_missing_plane_arg() {
    let bin = find_inferenctl();
    let out = Command::new(&bin).arg("inspect").output().unwrap();
    assert!(!out.status.success(), "Missing plane_id argument must fail");
}

#[test]
fn test_edge_cli_freeze_missing_lease_id_arg() {
    let bin = find_inferenctl();
    let out = Command::new(&bin).arg("freeze").output().unwrap();
    assert!(!out.status.success(), "Missing lease_id argument must fail");
}

#[test]
fn test_edge_cli_thaw_missing_lease_id_arg() {
    let bin = find_inferenctl();
    let out = Command::new(&bin).arg("thaw").output().unwrap();
    assert!(!out.status.success(), "Missing lease_id argument must fail");
}

#[test]
fn test_edge_cli_leases_unreachable_socket() {
    let bin = find_inferenctl();
    let out = Command::new(&bin)
        .args(["--socket", "/tmp/unreachable_socket_404.sock", "leases"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Failed to connect") || stderr.contains("No such file or directory"));
}

#[test]
fn test_edge_cli_dump_unreachable_socket() {
    let bin = find_inferenctl();
    let out = Command::new(&bin)
        .args(["--socket", "/tmp/unreachable_socket_404.sock", "dump"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Failed to connect") || stderr.contains("No such file or directory"));
}
