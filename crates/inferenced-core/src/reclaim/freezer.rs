use crate::error::{Error, Result};
use rustix::process::{kill_process, Pid, Signal};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Default cgroup root for systemd-inferenced managed slices
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Target state for cgroup.freeze
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeState {
    Frozen,
    Thawed,
}

/// Freeze a cgroups v2 hierarchy by writing "1" to cgroup.freeze
pub fn freeze_cgroup(slice_or_path: impl AsRef<Path>) -> Result<()> {
    write_cgroup_freeze(slice_or_path.as_ref(), "1")
}

/// Thaw a cgroups v2 hierarchy by writing "0" to cgroup.freeze
pub fn thaw_cgroup(slice_or_path: impl AsRef<Path>) -> Result<()> {
    write_cgroup_freeze(slice_or_path.as_ref(), "0")
}

/// Check if a cgroup hierarchy is currently frozen
pub fn is_cgroup_frozen(slice_or_path: impl AsRef<Path>) -> Result<bool> {
    let freeze_file = resolve_freeze_path(slice_or_path.as_ref());
    let content = fs::read_to_string(&freeze_file)
        .map_err(|e| Error::Freezer(format!("Failed to read {}: {}", freeze_file.display(), e)))?;
    Ok(content.trim() == "1")
}

/// Send a signal to a process using pure Rust rustix (SIGSTOP, SIGCONT, SIGUSR1)
pub fn signal_process(pid: u32, signal: Signal) -> Result<()> {
    let raw_pid = pid as i32;
    if let Some(proc_pid) = Pid::from_raw(raw_pid) {
        kill_process(proc_pid, signal).map_err(Error::SystemCall)?;
        Ok(())
    } else {
        Err(Error::Freezer(format!("Invalid PID: {}", pid)))
    }
}

/// Send cooperative preemption warning (SIGUSR1) allowing 250ms yield window
pub fn send_cooperative_yield_signal(pid: u32) -> Result<()> {
    signal_process(pid, Signal::Usr1)
}

/// Forcibly suspend a process via SIGSTOP (tier-2 preemption fallback)
pub fn freeze_process_signal(pid: u32) -> Result<()> {
    signal_process(pid, Signal::Stop)
}

/// Resume a suspended process via SIGCONT
pub fn thaw_process_signal(pid: u32) -> Result<()> {
    signal_process(pid, Signal::Cont)
}

fn resolve_freeze_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        if path.ends_with("cgroup.freeze") {
            path.to_path_buf()
        } else {
            path.join("cgroup.freeze")
        }
    } else {
        let direct = Path::new(CGROUP_ROOT).join(path).join("cgroup.freeze");
        if direct.exists() {
            return direct;
        }
        if let Some(s) = path.to_str() {
            if let Some((parent, _)) = s.split_once('-') {
                let nested = Path::new(CGROUP_ROOT)
                    .join(format!("{parent}.slice"))
                    .join(path)
                    .join("cgroup.freeze");
                if nested.exists() {
                    return nested;
                }
            }
        }
        direct
    }
}

fn write_cgroup_freeze(path: &Path, val: &str) -> Result<()> {
    let freeze_file = resolve_freeze_path(path);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .open(&freeze_file)
        .or_else(|_| File::create(&freeze_file))
        .map_err(|e| Error::Freezer(format!("Failed to open {}: {}", freeze_file.display(), e)))?;
    file.write_all(val.as_bytes())
        .map_err(|e| Error::Freezer(format!("Failed to write to {}: {}", freeze_file.display(), e)))?;
    file.flush()
        .map_err(|e| Error::Freezer(format!("Failed to flush {}: {}", freeze_file.display(), e)))?;
    Ok(())
}
