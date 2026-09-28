use inferenced_core::{
    fd_lease::{create_sealed_memfd, recv_fd_from_unix, send_fd_over_unix},
    madvise::{
        advise_dontdump, advise_dontneed, advise_hugepage, advise_random, advise_sequential,
        advise_willneed,
    },
};
use rustix::fs::{fcntl_get_seals, ftruncate, seek, SealFlags, SeekFrom};
use rustix::io::{read, write};
use rustix::mm::{mmap, munmap, MapFlags, ProtFlags};
use serde_json::json;
use std::ffi::c_void;
use std::os::unix::net::UnixStream;
use std::ptr;
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

#[test]
fn test_adversarial_sealed_memfd_kernel_seals_and_write_rejection() {
    let payload = b"SYSTEMD_INFERENCED_ZERO_COPY_WEIGHTS_4096";
    let fd = create_sealed_memfd("adv_sealed_model", payload.len() as u64, Some(payload))
        .expect("create_sealed_memfd must succeed");

    // 1. Verify kernel seals
    let seals = fcntl_get_seals(&fd).expect("fcntl_get_seals must succeed");
    assert!(seals.contains(SealFlags::SEAL));
    assert!(seals.contains(SealFlags::GROW));
    assert!(seals.contains(SealFlags::SHRINK));
    assert!(seals.contains(SealFlags::WRITE));

    // 2. Write to sealed memfd must fail with EPERM
    let write_res = write(&fd, b"MUTATION_ATTEMPT");
    assert!(write_res.is_err(), "Writing to sealed memfd must fail");

    // 3. Truncate of sealed memfd must fail with EPERM
    let trunc_res = ftruncate(&fd, 1024);
    assert!(trunc_res.is_err(), "Truncating sealed memfd must fail");
}

#[test]
fn test_adversarial_zero_copy_scm_rights_fd_passing_integrity() {
    let size = 1024 * 1024; // 1 MB
    let mut large_weights = vec![0u8; size];
    for (i, b) in large_weights.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }

    let weight_fd = create_sealed_memfd("large_model", size as u64, Some(&large_weights))
        .expect("create_sealed_memfd must succeed");

    let (s1, s2) = UnixStream::pair().expect("UnixStream pair");
    send_fd_over_unix(&s1, &weight_fd, b"model-adv-v1").expect("send_fd_over_unix");

    let mut buf = [0u8; 64];
    let (bytes_read, recvd_fd_opt) = recv_fd_from_unix(&s2, &mut buf).expect("recv_fd_from_unix");
    assert_eq!(&buf[..bytes_read], b"model-adv-v1");

    let recvd_fd = recvd_fd_opt.expect("Should receive open file descriptor");
    seek(&recvd_fd, SeekFrom::Start(0)).expect("seek to 0");

    let mut recvd_bytes = vec![0u8; size];
    let n = read(&recvd_fd, &mut recvd_bytes).expect("read from recvd fd");
    assert_eq!(n, size);
    assert_eq!(recvd_bytes, large_weights);
}

#[test]
fn test_adversarial_madvise_page_reclamation_and_coredump_exclusion() {
    let page_size = 4096;
    let len = page_size * 16; // 64 KB
    let data = vec![0x42u8; len];
    let fd = create_sealed_memfd("madvise_test", len as u64, Some(&data)).unwrap();

    let ptr = unsafe {
        mmap(
            ptr::null_mut(),
            len,
            ProtFlags::READ,
            MapFlags::SHARED,
            &fd,
            0,
        )
        .expect("mmap must succeed")
    };

    // Test kernel madvise operations
    assert!(advise_dontneed(ptr, len).is_ok());
    assert!(advise_willneed(ptr, len).is_ok());
    assert!(advise_hugepage(ptr, len).is_ok());
    assert!(advise_sequential(ptr, len).is_ok());
    assert!(advise_random(ptr, len).is_ok());
    assert!(advise_dontdump(ptr, len).is_ok());

    // Invalid pointer handling: unaligned pointer returns error (EINVAL) without panic
    let invalid_ptr = 1 as *mut c_void;
    assert!(advise_dontneed(invalid_ptr, 4096).is_err());

    unsafe {
        munmap(ptr, len).expect("munmap must succeed");
    }
}

#[tokio::test]
async fn test_adversarial_live_varlink_stream_piping() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("live_stream.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut reader = BufReader::new(r);
        let mut msg_buf = Vec::new();
        reader.read_until(0, &mut msg_buf).await.unwrap();

        let chunks = ["Chunk1: Hello ", "Chunk2: from ", "Chunk3: Varlink Stream!"];
        for (i, c) in chunks.iter().enumerate() {
            let continues = i < chunks.len() - 1;
            let rep = json!({
                "parameters": { "chunk": c },
                "continues": continues
            });
            let mut b = serde_json::to_vec(&rep).unwrap();
            b.push(0);
            w.write_all(&b).await.unwrap();
        }
    });

    let client_stream = tokio::net::UnixStream::connect(&sock).await.unwrap();
    let (r, mut w) = client_stream.into_split();
    let mut reader = BufReader::new(r);

    let req = json!({
        "method": "io.systemd.inferenced1.StreamInference",
        "parameters": { "model": "adv-model", "prompt": "stream prompt" },
        "more": true
    });
    let mut req_b = serde_json::to_vec(&req).unwrap();
    req_b.push(0);
    w.write_all(&req_b).await.unwrap();

    let mut stream_output = String::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(0, &mut buf).await.unwrap();
        if n == 0 { break; }
        let reply: serde_json::Value = serde_json::from_slice(&buf[..buf.len() - 1]).unwrap();
        if let Some(c) = reply["parameters"]["chunk"].as_str() {
            stream_output.push_str(c);
        }
        if !reply["continues"].as_bool().unwrap_or(false) { break; }
    }

    assert_eq!(stream_output, "Chunk1: Hello Chunk2: from Chunk3: Varlink Stream!");
}
