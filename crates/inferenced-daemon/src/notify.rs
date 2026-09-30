use rustix::fs::{fcntl_getfd, fcntl_setfd, FdFlags};
use rustix::net::{
    sendto_unix, socket, AddressFamily, SendFlags, SocketAddrUnix, SocketType,
};
use std::env;
use std::io;
use std::path::Path;
use tracing::{info, warn};

/// Maximum notify datagram size (systemd protocol cap).
const NOTIFY_MAX: usize = 8 * 1024 * 1024;

/// Build a `SocketAddrUnix` for the notify socket, supporting both
/// filesystem paths (`/run/systemd/notify`) and abstract namespaces
/// (`@notify`). Abstract names containing interior NULs are rejected
/// because the kernel would route to an address nobody listens on,
/// silently dropping the notification.
fn notify_address(socket_path: &str) -> Option<SocketAddrUnix> {
    if let Some(name) = socket_path.strip_prefix('@') {
        if name.is_empty() || name.as_bytes().contains(&0) {
            return None;
        }
        SocketAddrUnix::new_abstract_name(name.as_bytes()).ok()
    } else if socket_path.is_empty() {
        None
    } else {
        SocketAddrUnix::new(Path::new(socket_path)).ok()
    }
}

/// Strip embedded newlines so an attacker-supplied or caller-supplied
/// string cannot terminate the current sd_notify variable early and
/// inject a fake following variable (e.g. `MAINPID=`, `STOPPING=1`).
fn sanitize_value(s: &str) -> String {
    s.chars().filter(|&c| c != '\n' && c != '\r').collect()
}

/// Notify systemd that initialization has completed and service is ready
pub fn notify_systemd_ready() {
    if let Err(e) = send_notification("READY=1\nSTATUS=Hardware planes active\n") {
        warn!("Failed to send SD_NOTIFY READY=1: {}", e);
    } else {
        info!("Sent SD_NOTIFY READY=1 to systemd");
    }
}

/// Update systemd service status text shown in systemctl status.
/// Embedded newlines in `status` are stripped to prevent early
/// termination of the STATUS variable and injection of fake keys.
#[allow(dead_code)]
pub fn notify_systemd_status(status: &str) {
    let sanitized = sanitize_value(status);
    let msg = format!("STATUS={}\n", sanitized);
    if let Err(e) = send_notification(&msg) {
        warn!("Failed to send SD_NOTIFY status: {}", e);
    }
}

/// Keepalive ping for systemd WatchdogSec
#[allow(dead_code)]
pub fn notify_systemd_watchdog() {
    if let Err(e) = send_notification("WATCHDOG=1\n") {
        warn!("Failed to send SD_NOTIFY WATCHDOG: {}", e);
    }
}

/// Notify systemd that service is stopping
pub fn notify_systemd_stopping() {
    if let Err(e) = send_notification("STOPPING=1\nSTATUS=Shutting down cleanly\n") {
        warn!("Failed to send SD_NOTIFY STOPPING=1: {}", e);
    } else {
        info!("Sent SD_NOTIFY STOPPING=1 to systemd");
    }
}

/// Send a notification string to the socket defined in $NOTIFY_SOCKET.
/// Rejects payloads above the systemd 8 MiB cap with `InvalidInput`.
pub fn send_notification(state: &str) -> io::Result<usize> {
    if state.len() > NOTIFY_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "notification exceeds 8 MiB systemd cap",
        ));
    }
    let socket_path = match env::var("NOTIFY_SOCKET") {
        Ok(path) if !path.is_empty() => path,
        _ => return Ok(0),
    };
    send_notification_to(&socket_path, state)
}

/// Send notification payload directly to specified socket (abstract or filesystem)
pub fn send_notification_to(socket_path: &str, state: &str) -> io::Result<usize> {
    let addr = notify_address(socket_path).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid NOTIFY_SOCKET target '{}'", socket_path),
        )
    })?;

    let sock = socket(AddressFamily::UNIX, SocketType::DGRAM, None)
        .map_err(io::Error::other)?;

    if let Ok(flags) = fcntl_getfd(&sock) {
        let _ = fcntl_setfd(&sock, flags | FdFlags::CLOEXEC);
    }

    sendto_unix(&sock, state.as_bytes(), SendFlags::empty(), &addr)
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixDatagram};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_COUNTER: AtomicUsize = AtomicUsize::new(100);

    fn create_test_abstract_socket() -> (UnixDatagram, String) {
        let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let name = format!("inferenced_notify_mod_test_{}_{}", std::process::id(), id);
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).expect("abstract addr");
        let receiver = UnixDatagram::bind_addr(&addr).expect("bind abstract");
        (receiver, format!("@{}", name))
    }

    #[test]
    fn test_abstract_notify_all_messages() {
        let (receiver, addr_str) = create_test_abstract_socket();

        // 1. Ready
        let ready_msg = "READY=1\nSTATUS=Hardware planes active\n";
        let sent = send_notification_to(&addr_str, ready_msg).expect("send ready");
        assert_eq!(sent, ready_msg.len());

        let mut buf = [0u8; 512];
        let (n, _) = receiver.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], ready_msg.as_bytes());

        // 2. Status
        let status_msg = "STATUS=Planes active: 3\n";
        send_notification_to(&addr_str, status_msg).unwrap();
        let (n, _) = receiver.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], status_msg.as_bytes());

        // 3. Watchdog
        let wd_msg = "WATCHDOG=1\n";
        send_notification_to(&addr_str, wd_msg).unwrap();
        let (n, _) = receiver.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], wd_msg.as_bytes());

        // 4. Stopping
        let stop_msg = "STOPPING=1\nSTATUS=Shutting down cleanly\n";
        send_notification_to(&addr_str, stop_msg).unwrap();
        let (n, _) = receiver.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], stop_msg.as_bytes());
    }

    #[test]
    fn test_notify_missing_env_noop() {
        env::remove_var("NOTIFY_SOCKET");
        let res = send_notification("READY=1\n");
        assert_eq!(res.unwrap(), 0);
    }

    #[test]
    fn test_notify_status_strips_embedded_newlines() {
        // An attacker-supplied or caller-supplied status string with an
        // embedded newline must not be able to terminate the STATUS
        // variable early and inject a fake following variable.
        let sanitized = super::sanitize_value("evil\nMAINPID=42\n");
        assert_eq!(sanitized, "evilMAINPID=42");
        assert!(!sanitized.contains('\n'));
        assert!(!sanitized.contains('\r'));
    }

    #[test]
    fn test_notify_oversize_payload_rejected() {
        let huge = "X".repeat(super::NOTIFY_MAX + 1);
        let res = send_notification(&huge);
        assert!(res.is_err(), "oversize payload must be rejected");
    }

    #[test]
    fn test_notify_rejects_interior_nul_in_abstract_name() {
        // Abstract names containing interior NULs would silently route
        // to an address nobody listens on.
        let res = send_notification_to("@notify\0anything", "READY=1\n");
        assert!(res.is_err(), "interior-NUL abstract name must be rejected");
    }

    #[test]
    fn test_notify_rejects_empty_abstract_name() {
        let res = send_notification_to("@", "READY=1\n");
        assert!(res.is_err(), "empty abstract name must be rejected");
    }
}
