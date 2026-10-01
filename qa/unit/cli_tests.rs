use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use tempfile::NamedTempFile;

fn find_inferenctl() -> PathBuf {
    if let Ok(cargo_bin) = std::env::var("CARGO_BIN_EXE_inferenctl") {
        return PathBuf::from(cargo_bin);
    }
    let mut path = std::env::current_exe().unwrap_or_default();
    while path.pop() {
        if path.file_name().map(|n| n == "deps").unwrap_or(false) {
            if let Some(parent) = path.parent() {
                let candidate = parent.join("inferenctl");
                if candidate.exists() {
                    return candidate;
                }
            }
        }
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
fn test_cli_version_flag() {
    let bin = find_inferenctl();
    let out = Command::new(&bin).arg("--version").output().expect("inferenctl --version");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(concat!("inferenctl ", env!("CARGO_PKG_VERSION"))));
}

#[test]
fn test_cli_help_flag_displays_subcommands() {
    let bin = find_inferenctl();
    let out = Command::new(&bin).arg("--help").output().expect("inferenctl --help");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("status"));
    assert!(stdout.contains("planes"));
    assert!(stdout.contains("leases"));
    assert!(stdout.contains("exec"));
    assert!(stdout.contains("cat-config"));
    assert!(stdout.contains("check-config"));
    assert!(stdout.contains("completions"));
    assert!(stdout.contains("man"));
}

#[test]
fn test_cli_completions_bash() {
    let bin = find_inferenctl();
    let out = Command::new(&bin).args(["completions", "bash"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("_inferenctl()"));
    assert!(stdout.contains("complete -F _inferenctl inferenctl"));
}

#[test]
fn test_cli_completions_zsh() {
    let bin = find_inferenctl();
    let out = Command::new(&bin).args(["completions", "zsh"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("#compdef inferenctl"));
}

#[test]
fn test_cli_completions_fish() {
    let bin = find_inferenctl();
    let out = Command::new(&bin).args(["completions", "fish"]).output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("# fish completion for inferenctl"));
    assert!(stdout.contains("complete -c inferenctl"));
}

#[test]
fn test_cli_man_page_generation() {
    let bin = find_inferenctl();
    let out = Command::new(&bin).arg("man").output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(".TH INFERENCTL 1"));
    assert!(stdout.contains(".SH NAME"));
    assert!(stdout.contains(".SH COMMANDS"));
}

#[test]
fn test_cli_cat_config_missing_file_shows_defaults() {
    let bin = find_inferenctl();
    let out = Command::new(&bin)
        .args(["cat-config", "--config", "/tmp/nonexistent_cfg_123.conf"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("[daemon]"));
    assert!(stdout.contains("gateway_socket = \"/run/syntrop/gateway.sock\""));
}

#[test]
fn test_cli_cat_config_existing_file() {
    let bin = find_inferenctl();
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "# Custom test config\n[arbiter]\nmode = \"aggressive\"").unwrap();

    let out = Command::new(&bin)
        .args(["cat-config", "--config", file.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("mode = \"aggressive\""));
}

#[test]
fn test_cli_check_config_valid_toml() {
    let bin = find_inferenctl();
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "[daemon]\nbind = \"127.0.0.1:8080\"\n\n[arbiter]\nmax_leases = 10").unwrap();

    let out = Command::new(&bin)
        .args(["check-config", "--config", file.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("is valid syntax TOML"));
}

#[test]
fn test_cli_check_config_syntax_error() {
    let bin = find_inferenctl();
    let mut file = NamedTempFile::new().unwrap();
    writeln!(file, "[invalid toml syntax\nkey = = =").unwrap();

    let out = Command::new(&bin)
        .args(["check-config", "--config", file.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(!out.status.success());
}

fn find_inferenced() -> PathBuf {
    if let Ok(cargo_bin) = std::env::var("CARGO_BIN_EXE_inferenced") {
        return PathBuf::from(cargo_bin);
    }
    let mut path = std::env::current_exe().unwrap_or_default();
    while path.pop() {
        if path.file_name().map(|n| n == "target").unwrap_or(false) {
            let candidate = path.join("debug/inferenced");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("target/debug/inferenced")
}

#[test]
fn test_inferenced_cli_help_options() {
    let bin = find_inferenced();
    let out = Command::new(&bin).arg("--help").output().expect("inferenced --help");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--gateway-socket"));
    assert!(stdout.contains("/run/syntrop/gateway.sock"));
    assert!(stdout.contains("--bind"));
    assert!(!stdout.contains("11434"));
}
