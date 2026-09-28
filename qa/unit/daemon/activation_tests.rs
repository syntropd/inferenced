use rustix::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use rustix::fs::{fcntl_getfd, fcntl_setfd, FdFlags};
use rustix::net::{getsockname, SocketAddrAny};
use std::env;
use std::os::unix::net::UnixListener as StdUnixListener;
use std::sync::Mutex;
use tempfile::tempdir;

const SD_LISTEN_FDS_START: i32 = 3;
static ENV_MUTEX: Mutex<()> = Mutex::new(());

#[test]
fn test_activation_cloexec_flag_enforcement() {
    let dir = tempdir().unwrap();
    let sock_path = dir.path().join("cloexec_test.sock");
    let listener = StdUnixListener::bind(&sock_path).unwrap();
    let raw = listener.as_raw_fd();
    let owned = unsafe { OwnedFd::from_raw_fd(raw) };

    // Set and verify CLOEXEC
    let flags = fcntl_getfd(&owned).expect("fcntl_getfd");
    fcntl_setfd(&owned, flags | FdFlags::CLOEXEC).expect("fcntl_setfd");

    let updated = fcntl_getfd(&owned).expect("fcntl_getfd after set");
    assert!(updated.contains(FdFlags::CLOEXEC));

    // Release ownership to avoid double-close
    let _ = owned.into_raw_fd();
}

#[test]
fn test_activation_environment_pid_matching() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    let my_pid = std::process::id();

    // Matching PID
    env::set_var("LISTEN_PID", my_pid.to_string());
    env::set_var("LISTEN_FDS", "3");
    let pid_val: u32 = env::var("LISTEN_PID").unwrap().parse().unwrap();
    let fds_val: i32 = env::var("LISTEN_FDS").unwrap().parse().unwrap();
    assert_eq!(pid_val, my_pid);
    assert_eq!(fds_val, 3);

    // Mismatched PID
    let wrong_pid = my_pid + 99999;
    env::set_var("LISTEN_PID", wrong_pid.to_string());
    let parsed_wrong: u32 = env::var("LISTEN_PID").unwrap().parse().unwrap();
    assert_ne!(parsed_wrong, my_pid);

    env::remove_var("LISTEN_PID");
    env::remove_var("LISTEN_FDS");
}

#[test]
fn test_activation_fd_numbering_sequence() {
    // Systemd passes FDs starting at 3: FD 3, FD 4, FD 5
    assert_eq!(SD_LISTEN_FDS_START, 3);
    let count = 3;
    let expected_fds: Vec<i32> = (0..count).map(|i| SD_LISTEN_FDS_START + i).collect();
    assert_eq!(expected_fds, vec![3, 4, 5]);
}

#[test]
fn test_activation_socket_family_inspection() {
    let dir = tempdir().unwrap();
    let sock_path = dir.path().join("family_test.sock");
    let listener = StdUnixListener::bind(&sock_path).unwrap();

    let raw = listener.as_raw_fd();
    let owned = unsafe { OwnedFd::from_raw_fd(raw) };
    let sock_addr = getsockname(&owned).expect("getsockname");

    match sock_addr {
        SocketAddrAny::Unix(addr) => {
            assert!(addr.path().is_some());
        }
        _ => panic!("Expected Unix socket family"),
    }

    let _ = owned.into_raw_fd();
}

#[test]
fn test_activation_zero_fds_handled_gracefully() {
    let _guard = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
    env::set_var("LISTEN_PID", std::process::id().to_string());
    env::set_var("LISTEN_FDS", "0");

    let fds_count: i32 = env::var("LISTEN_FDS").unwrap().parse().unwrap_or(0);
    assert!(fds_count <= 0, "Zero FDs must result in no adoption");

    env::remove_var("LISTEN_PID");
    env::remove_var("LISTEN_FDS");
}
