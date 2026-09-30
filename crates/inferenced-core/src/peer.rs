use crate::error::{Error, Result};
use rustix::net::sockopt::get_socket_peercred;
use std::fs;
use std::os::unix::io::AsFd;

/// Information extracted from socket peer credentials and Linux cgroup v2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
    pub cgroup_path: String,
    pub slice: String,
    pub is_container: bool,
    pub is_batch: bool,
}

impl PeerInfo {
    /// Retrieve verified peer credentials directly from the connected UNIX socket
    /// and attribute its cgroup v2 slice from /proc/<pid>/cgroup.
    pub fn from_socket<F: AsFd>(fd: &F) -> Result<Self> {
        let ucred = get_socket_peercred(fd).map_err(|e| {
            Error::Systemd(format!("Failed to retrieve SO_PEERCRED: {}", e))
        })?;

        let raw_pid = rustix::process::Pid::as_raw(Some(ucred.pid));
        let pid = if raw_pid > 0 { raw_pid as u32 } else { 0 };
        let uid = ucred.uid.as_raw();
        let gid = ucred.gid.as_raw();

        let cgroup_path = if pid > 0 {
            Self::read_cgroup_path(pid).unwrap_or_else(|| "/system.slice".to_string())
        } else {
            "/system.slice".to_string()
        };

        let (slice, is_container, is_batch) = Self::parse_slice_and_flags(&cgroup_path);

        Ok(Self {
            pid,
            uid,
            gid,
            cgroup_path,
            slice,
            is_container,
            is_batch,
        })
    }

    /// Read cgroup path from `/proc/<pid>/cgroup` (bounded to 512 bytes).
    pub fn read_cgroup_path(pid: u32) -> Option<String> {
        let proc_path = format!("/proc/{}/cgroup", pid);
        let mut file = fs::File::open(&proc_path).ok()?;
        let mut buf = [0u8; 512];
        let n = std::io::Read::read(&mut file, &mut buf).ok()?;
        let content = std::str::from_utf8(&buf[..n]).ok()?;
        // cgroup v2 format: "0::<cgroup_path>"
        for line in content.lines() {
            if let Some(path) = line.strip_prefix("0::") {
                let trimmed = path.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }

    /// Parse slice name and attributes from a cgroup v2 path.
    pub fn parse_slice_and_flags(cgroup_path: &str) -> (String, bool, bool) {
        let is_container = cgroup_path.contains("machine.slice");
        let is_batch = cgroup_path.contains("ai-batch.slice");

        let default_slice = if is_container {
            "machine.slice"
        } else if is_batch {
            "ai-batch.slice"
        } else {
            "system.slice"
        };

        // Extract first .slice segment in path
        let slice = cgroup_path
            .split('/')
            .find(|seg| seg.ends_with(".slice"))
            .unwrap_or(default_slice)
            .to_string();

        (slice, is_container, is_batch)
    }

    /// Construct a mock PeerInfo for unit testing.
    pub fn mock(pid: u32, uid: u32, gid: u32, cgroup_path: &str) -> Self {
        let (slice, is_container, is_batch) = Self::parse_slice_and_flags(cgroup_path);
        Self {
            pid,
            uid,
            gid,
            cgroup_path: cgroup_path.to_string(),
            slice,
            is_container,
            is_batch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_slice_and_flags() {
        let (slice, is_container, is_batch) =
            PeerInfo::parse_slice_and_flags("/user.slice/user-1000.slice/session-1.scope");
        assert_eq!(slice, "user.slice");
        assert!(!is_container);
        assert!(!is_batch);

        let (slice, is_container, is_batch) =
            PeerInfo::parse_slice_and_flags("/machine.slice/machine-fedora.scope");
        assert_eq!(slice, "machine.slice");
        assert!(is_container);
        assert!(!is_batch);

        let (slice, is_container, is_batch) =
            PeerInfo::parse_slice_and_flags("/ai.slice/ai-batch.slice/job-42.service");
        assert_eq!(slice, "ai.slice");
        assert!(!is_container);
        assert!(is_batch);
    }

    #[test]
    fn test_mock_peer_info() {
        let info = PeerInfo::mock(1234, 1000, 1000, "/machine.slice/container.scope");
        assert_eq!(info.pid, 1234);
        assert_eq!(info.uid, 1000);
        assert!(info.is_container);
    }
}
