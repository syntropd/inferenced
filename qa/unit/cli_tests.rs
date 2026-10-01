use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use tempfile::NamedTempFile;

static BUILD_CTL_ONCE: std::sync::Once = std::sync::Once::new();
static BUILD_DAEMON_ONCE: std::sync::Once = std::sync::Once::new();

fn workspace_root() -> PathBuf {
    let mut path = std::env::current_exe().unwrap_or_default();
    while path.pop() {
        if path.join("Cargo.toml").exists() && path.join("crates/inferenctl").exists() {
            return path;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn is_stale_bin(bin: &std::path::Path, expected_ver: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .output()
        .map(|o| !o.status.success() || !String::from_utf8_lossy(&o.stdout).contains(expected_ver))
        .unwrap_or(true)
}

fn find_inferenctl() -> PathBuf {
    if let Ok(cargo_bin) = std::env::var("CARGO_BIN_EXE_inferenctl") {
        let p = PathBuf::from(cargo_bin);
        if p.exists() && !is_stale_bin(&p, env!("CARGO_PKG_VERSION")) {
            return p;
        }
    }
    let root = workspace_root();
    let default_candidate = if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        PathBuf::from(dir).join("debug/inferenctl")
    } else {
        root.join("target/debug/inferenctl")
    };
    if !default_candidate.exists() || is_stale_bin(&default_candidate, env!("CARGO_PKG_VERSION")) {
        BUILD_CTL_ONCE.call_once(|| {
            let _ = std::fs::remove_file(&default_candidate);
            let manifest = root.join("Cargo.toml");
            let _ = Command::new("cargo")
                .args(["build", "-q", "--manifest-path"])
                .arg(&manifest)
                .args(["-p", "inferenctl", "--bin", "inferenctl"])
                .status();
        });
    }
    if default_candidate.exists() {
        return default_candidate;
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
    }
    default_candidate
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
        let p = PathBuf::from(cargo_bin);
        if p.exists() && !is_stale_bin(&p, env!("CARGO_PKG_VERSION")) {
            return p;
        }
    }
    let root = workspace_root();
    let default_candidate = if let Ok(dir) = std::env::var("CARGO_TARGET_DIR") {
        PathBuf::from(dir).join("debug/inferenced")
    } else {
        root.join("target/debug/inferenced")
    };
    if !default_candidate.exists() || is_stale_bin(&default_candidate, env!("CARGO_PKG_VERSION")) {
        BUILD_DAEMON_ONCE.call_once(|| {
            let _ = std::fs::remove_file(&default_candidate);
            let manifest = root.join("Cargo.toml");
            let _ = Command::new("cargo")
                .args(["build", "-q", "--manifest-path"])
                .arg(&manifest)
                .args(["--bin", "inferenced"])
                .status();
        });
    }
    if default_candidate.exists() {
        return default_candidate;
    }
    let mut path = std::env::current_exe().unwrap_or_default();
    while path.pop() {
        if path.file_name().map(|n| n == "deps").unwrap_or(false) {
            if let Some(parent) = path.parent() {
                let candidate = parent.join("inferenced");
                if candidate.exists() {
                    return candidate;
                }
            }
        }
    }
    default_candidate
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
