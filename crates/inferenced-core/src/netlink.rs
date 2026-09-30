use crate::error::{Error, Result};
use rustix::fd::{AsRawFd, OwnedFd};
use rustix::net::{netlink, socket_with, AddressFamily, SocketFlags, SocketType};
use std::collections::HashMap;

/// Parsed kernel device event from Linux Netlink KOBJECT_UEVENT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uevent {
    pub action: String,
    pub devpath: String,
    pub subsystem: String,
    pub devname: Option<String>,
    pub properties: HashMap<String, String>,
}

impl Uevent {
    /// Parse a raw netlink uevent buffer (NUL-delimited strings).
    pub fn parse(buf: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(buf).ok()?;
        let mut lines = text.split('\0').filter(|s| !s.is_empty());
        let header = lines.next()?; // e.g. "add@/devices/.../drm/renderD128"
        let (action_str, devpath_str) = header.split_once('@')?;

        let mut properties = HashMap::new();
        let mut subsystem = String::new();
        let mut devname = None;

        for prop in lines {
            if let Some((k, v)) = prop.split_once('=') {
                match k {
                    "SUBSYSTEM" => subsystem = v.to_string(),
                    "DEVNAME" => devname = Some(v.to_string()),
                    _ => {}
                }
                properties.insert(k.to_string(), v.to_string());
            }
        }

        if subsystem.is_empty() {
            if let Some(sub) = properties.get("SUBSYSTEM") {
                subsystem = sub.clone();
            }
        }

        Some(Self {
            action: action_str.to_string(),
            devpath: devpath_str.to_string(),
            subsystem,
            devname,
            properties,
        })
    }

    /// Returns true if this event corresponds to an AI compute plane (DRM GPU, NPU, Hailo).
    pub fn is_compute_device(&self) -> bool {
        self.subsystem == "drm"
            || self.subsystem == "accel"
            || (self.subsystem == "misc" && self.devname.as_deref().is_some_and(|n| n.contains("hailo")))
            || self.subsystem == "kfd"
    }

    /// Returns true if the event represents an ASIC reset or driver timeout event.
    pub fn is_reset_event(&self) -> bool {
        self.action == "change"
            && self.properties.get("RESET").is_some_and(|v| v == "1")
    }
}

/// Open and bind a non-blocking Netlink KOBJECT_UEVENT multicast listener socket.
pub fn open_uevent_socket() -> Result<OwnedFd> {
    let fd = socket_with(
        AddressFamily::NETLINK,
        SocketType::RAW,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        Some(netlink::KOBJECT_UEVENT),
    )
    .map_err(|e| Error::Systemd(format!("Failed to create AF_NETLINK socket: {e}")))?;

    let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    addr.nl_pid = 0;
    addr.nl_groups = 1;

    let res = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    };

    if res < 0 {
        return Err(Error::Systemd(format!(
            "Failed to bind netlink socket to group 1: {}",
            std::io::Error::last_os_error()
        )));
    }

    Ok(fd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_drm_add_uevent() {
        let raw = b"add@/devices/pci0000:00/0000:00:01.0/drm/renderD128\0ACTION=add\0DEVPATH=/devices/pci0000:00/0000:00:01.0/drm/renderD128\0SUBSYSTEM=drm\0DEVNAME=/dev/dri/renderD128\0MAJOR=226\0MINOR=128\0";
        let uevent = Uevent::parse(raw).unwrap();
        assert_eq!(uevent.action, "add");
        assert_eq!(uevent.subsystem, "drm");
        assert_eq!(uevent.devname.as_deref(), Some("/dev/dri/renderD128"));
        assert!(uevent.is_compute_device());
        assert!(!uevent.is_reset_event());
    }

    #[test]
    fn test_parse_reset_event() {
        let raw = b"change@/devices/pci0000:00/0000:00:01.0/drm/card0\0ACTION=change\0SUBSYSTEM=drm\0RESET=1\0";
        let uevent = Uevent::parse(raw).unwrap();
        assert_eq!(uevent.action, "change");
        assert!(uevent.is_reset_event());
    }

    #[test]
    fn test_compute_device_subsystems() {
        let accel_raw = b"add@/devices/pci/accel/accel0\0SUBSYSTEM=accel\0DEVNAME=/dev/accel/accel0\0";
        let uevent = Uevent::parse(accel_raw).unwrap();
        assert!(uevent.is_compute_device());

        let hailo_raw = b"add@/devices/pci/misc/hailo0\0SUBSYSTEM=misc\0DEVNAME=/dev/hailo0\0";
        let uevent = Uevent::parse(hailo_raw).unwrap();
        assert!(uevent.is_compute_device());

        let kfd_raw = b"add@/devices/virtual/kfd\0SUBSYSTEM=kfd\0";
        let uevent = Uevent::parse(kfd_raw).unwrap();
        assert!(uevent.is_compute_device());

        let input_raw = b"add@/devices/input/input0\0SUBSYSTEM=input\0DEVNAME=/dev/input/event0\0";
        let uevent = Uevent::parse(input_raw).unwrap();
        assert!(!uevent.is_compute_device());
    }

    #[test]
    fn test_parse_malformed_uevent() {
        assert!(Uevent::parse(b"").is_none());
        assert!(Uevent::parse(b"malformed_without_delimiter").is_none());
        assert!(Uevent::parse(b"\0\0").is_none());
    }

    #[test]
    fn test_open_uevent_socket_lifecycle() {
        if let Ok(fd) = open_uevent_socket() {
            assert!(fd.as_raw_fd() >= 0);
            let flags = rustix::fs::fcntl_getfl(&fd).expect("fcntl_getfl should succeed");
            assert!(flags.contains(rustix::fs::OFlags::NONBLOCK));
        }
    }
}
