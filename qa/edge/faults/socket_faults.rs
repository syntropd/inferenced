use inferenced_core::fd_lease::recv_fd_from_unix;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

#[test]
fn test_edge_connect_nonexistent_unix_socket() {
    let non_existent = "/run/systemd-inferenced/non_existent_test_404.sock";
    let res = UnixStream::connect(Path::new(non_existent));
    assert!(res.is_err(), "Connecting to non-existent socket must fail");
}

#[test]
fn test_edge_read_from_closed_stream_returns_zero() {
    let (s1, mut s2) = UnixStream::pair().unwrap();
    // Drop sender immediately
    drop(s1);

    let mut buf = [0u8; 64];
    let n = s2.read(&mut buf).unwrap();
    assert_eq!(n, 0, "Reading from closed stream must return EOF (0 bytes)");
}

#[test]
fn test_edge_write_to_closed_stream_errors() {
    let (mut s1, s2) = UnixStream::pair().unwrap();
    drop(s2);

    let res = s1.write_all(b"payload to closed peer");
    assert!(res.is_err(), "Writing to closed peer must return broken pipe / error");
}

#[test]
fn test_edge_recv_fd_when_none_sent() {
    let (mut s1, s2) = UnixStream::pair().unwrap();
    // Send plain bytes without ancillary SCM_RIGHTS descriptor
    s1.write_all(b"plain-message").unwrap();

    let mut buf = [0u8; 64];
    let (n, maybe_fd) = recv_fd_from_unix(&s2, &mut buf).unwrap();
    assert_eq!(&buf[..n], b"plain-message");
    assert!(maybe_fd.is_none(), "No FD was sent, received_fd must be None");
}

#[test]
fn test_edge_bind_unwritable_directory_fails() {
    let invalid_path = "/root/unwritable_test_systemd_inferenced.sock";
    let res = UnixListener::bind(invalid_path);
    // Since we are unprivileged, binding to /root must fail with EACCES or ENOENT
    assert!(res.is_err(), "Binding to /root must fail for unprivileged user");
}

#[test]
fn test_edge_large_payload_stream_chunking() {
    let (mut s1, mut s2) = UnixStream::pair().unwrap();
    let large_data = vec![0x55u8; 128 * 1024]; // 128 KB

    std::thread::spawn(move || {
        s1.write_all(&large_data).unwrap();
    });

    let mut received = Vec::new();
    let mut chunk = [0u8; 4096];
    while received.len() < 128 * 1024 {
        let n = s2.read(&mut chunk).unwrap();
        if n == 0 { break; }
        received.extend_from_slice(&chunk[..n]);
    }
    assert_eq!(received.len(), 128 * 1024);
}
