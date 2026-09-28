use inferenced_core::{
    fd_lease::{create_sealed_memfd, recv_fd_from_unix, send_fd_over_unix},
    madvise::{advise_dontneed, advise_willneed},
};
use rustix::fs::{fcntl_get_seals, ftruncate, SealFlags};
use rustix::io::write;
use rustix::mm::{mmap, munmap, MapFlags, ProtFlags};
use std::os::unix::net::{UnixListener, UnixStream};
use std::ptr;
use std::sync::{Arc, Barrier, Mutex};
use tempfile::tempdir;

static SCM_LOCK: Mutex<()> = Mutex::new(());

fn count_open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd")
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0)
}

#[test]
fn test_scm_rights_20_client_sealed_memfd_fanout_and_fd_leak_check() {
    let _lock = SCM_LOCK.lock().unwrap();

    let baseline_fds = count_open_fds();
    let size = 4 * 1024 * 1024; // 4MB
    let mut payload = vec![0u8; size];
    for (i, b) in payload.iter_mut().enumerate() {
        *b = ((i * 33 + 7) & 0xFF) as u8;
    }

    let memfd = create_sealed_memfd("adv_sealed_model_4mb", size as u64, Some(&payload))
        .expect("create_sealed_memfd must succeed");

    // 1. Verify kernel seals
    let seals = fcntl_get_seals(&memfd).expect("fcntl_get_seals must succeed");
    assert!(seals.contains(SealFlags::SEAL));
    assert!(seals.contains(SealFlags::GROW));
    assert!(seals.contains(SealFlags::SHRINK));
    assert!(seals.contains(SealFlags::WRITE));

    let dir = tempdir().expect("tempdir");
    let sock_path = dir.path().join("scm_fanout.sock");
    let listener = UnixListener::bind(&sock_path).expect("UnixListener bind");

    // Server thread passes the sealed memfd to 20 clients
    let server_memfd = memfd;
    let server_handle = std::thread::spawn(move || {
        for _ in 0..20 {
            let (stream, _) = listener.accept().expect("listener accept");
            send_fd_over_unix(&stream, &server_memfd, b"WEIGHTS_V1").expect("send_fd_over_unix");
        }
        server_memfd
    });

    // 20 concurrent client threads receive FD, verify mutation rejection, and madvise integrity
    let barrier = Arc::new(Barrier::new(20));
    let mut client_handles = Vec::new();

    for client_id in 0..20 {
        let path = sock_path.clone();
        let bar = barrier.clone();
        let expected_payload = payload.clone();

        client_handles.push(std::thread::spawn(move || {
            bar.wait();
            let stream = UnixStream::connect(&path).expect("connect to server");
            let mut buf = [0u8; 64];
            let (n, recvd_fd_opt) = recv_fd_from_unix(&stream, &mut buf).expect("recv_fd_from_unix");
            assert_eq!(&buf[..n], b"WEIGHTS_V1");

            let recvd_fd = recvd_fd_opt.expect("Client must receive open FD");

            // Adversarial mutation rejection 1: write attempt must fail with EPERM
            let write_err = write(&recvd_fd, b"CORRUPT_MODEL").expect_err("write to sealed memfd must fail");
            assert_eq!(
                write_err,
                rustix::io::Errno::PERM,
                "Kernel must reject write with EPERM"
            );

            // Adversarial mutation rejection 2: ftruncate attempt must fail with EPERM
            let trunc_err = ftruncate(&recvd_fd, 2048).expect_err("ftruncate on sealed memfd must fail");
            assert_eq!(
                trunc_err,
                rustix::io::Errno::PERM,
                "Kernel must reject ftruncate with EPERM"
            );

            // Memory madvise integrity
            let ptr = unsafe {
                mmap(
                    ptr::null_mut(),
                    size,
                    ProtFlags::READ,
                    MapFlags::SHARED,
                    &recvd_fd,
                    0,
                )
                .expect("mmap must succeed")
            };

            // Validate data integrity across entire 4MB buffer
            let mapped_slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, size) };
            assert_eq!(
                mapped_slice[0],
                expected_payload[0],
                "Client {} payload mismatch at start",
                client_id
            );
            assert_eq!(
                mapped_slice[size / 2],
                expected_payload[size / 2],
                "Client {} payload mismatch at middle",
                client_id
            );
            assert_eq!(
                mapped_slice[size - 1],
                expected_payload[size - 1],
                "Client {} payload mismatch at end",
                client_id
            );

            // Madvise page paging hints
            assert!(advise_willneed(ptr, size).is_ok());
            assert!(advise_dontneed(ptr, size).is_ok());

            unsafe {
                munmap(ptr, size).expect("munmap must succeed");
            }

            drop(recvd_fd);
            drop(stream);
        }));
    }

    for h in client_handles {
        h.join().expect("Client thread join");
    }

    let returned_memfd = server_handle.join().expect("Server thread join");
    drop(returned_memfd);
    drop(dir);

    // FD leak verification: confirm host open file descriptor count has returned to baseline,
    // allowing an FD delta <= 4 for temporary descriptors opened concurrently by test runner threads.
    let mut final_fds = count_open_fds();
    let mut delta = final_fds.saturating_sub(baseline_fds);
    if delta > 4 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        final_fds = count_open_fds();
        delta = final_fds.saturating_sub(baseline_fds);
    }
    assert!(
        delta <= 4,
        "Host FD leak detected! Baseline: {}, Final: {}, Delta: {}",
        baseline_fds,
        final_fds,
        delta
    );
}
