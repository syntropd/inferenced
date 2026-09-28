use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(1);

fn create_abstract_receiver() -> (UnixDatagram, String) {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let name = format!("inferenced_notify_test_{}_{}", std::process::id(), id);
    let addr = SocketAddr::from_abstract_name(name.as_bytes()).expect("abstract addr");
    let receiver = UnixDatagram::bind_addr(&addr).expect("bind abstract");
    (receiver, format!("@{}", name))
}

fn send_test_notification(notify_socket: &str, payload: &str) -> std::io::Result<usize> {
    let sock = UnixDatagram::unbound()?;
    let addr = if let Some(stripped) = notify_socket.strip_prefix('@') {
        SocketAddr::from_abstract_name(stripped.as_bytes())?
    } else {
        SocketAddr::from_pathname(notify_socket)?
    };
    sock.send_to_addr(payload.as_bytes(), &addr)
}

#[test]
fn test_notify_abstract_socket_ready() {
    let (receiver, addr_str) = create_abstract_receiver();
    let payload = "READY=1\nSTATUS=Hardware planes active\n";

    send_test_notification(&addr_str, payload).expect("send ready");

    let mut buf = [0u8; 256];
    let (n, _) = receiver.recv_from(&mut buf).expect("receive ready");
    assert_eq!(&buf[..n], payload.as_bytes());
}

#[test]
fn test_notify_abstract_socket_status_update() {
    let (receiver, addr_str) = create_abstract_receiver();
    let payload = "STATUS=Leases: 4 active, PSI: Nominal\n";

    send_test_notification(&addr_str, payload).expect("send status");

    let mut buf = [0u8; 256];
    let (n, _) = receiver.recv_from(&mut buf).expect("receive status");
    assert_eq!(&buf[..n], payload.as_bytes());
}

#[test]
fn test_notify_abstract_socket_watchdog_ping() {
    let (receiver, addr_str) = create_abstract_receiver();
    let payload = "WATCHDOG=1\n";

    send_test_notification(&addr_str, payload).expect("send watchdog");

    let mut buf = [0u8; 256];
    let (n, _) = receiver.recv_from(&mut buf).expect("receive watchdog");
    assert_eq!(&buf[..n], payload.as_bytes());
}

#[test]
fn test_notify_abstract_socket_stopping() {
    let (receiver, addr_str) = create_abstract_receiver();
    let payload = "STOPPING=1\nSTATUS=Shutting down cleanly\n";

    send_test_notification(&addr_str, payload).expect("send stopping");

    let mut buf = [0u8; 256];
    let (n, _) = receiver.recv_from(&mut buf).expect("receive stopping");
    assert_eq!(&buf[..n], payload.as_bytes());
}

#[test]
fn test_notify_missing_or_invalid_socket_graceful() {
    // Sending to a non-existent abstract socket must gracefully error without panicking
    let res = send_test_notification("@nonexistent_systemd_notify_12345", "READY=1\n");
    // Depending on Linux kernel behavior, unconnected datagram send to non-listening abstract socket returns ECONNREFUSED or ENOENT
    assert!(res.is_err());
}
