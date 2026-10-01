//! Integration tests for the FD handoff server.

use crate::fd_quota::FdQuota;
use crate::fd_server::{run_fd_server, run_fd_server_with_quota, FdResponse, MAX_MEMFD_BYTES};
use inferenced_core::fd_lease::recv_fd_from_unix;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

async fn write_framed(client: &mut UnixStream, json: serde_json::Value) {
    let mut msg = serde_json::to_vec(&json).unwrap();
    msg.push(0x00);
    client.write_all(&msg).await.unwrap();
    client.flush().await.unwrap();
}

async fn read_framed_response(client: &mut UnixStream) -> FdResponse {
    let mut buf = vec![0u8; 512];
    let n = client.read(&mut buf).await.unwrap();
    let trimmed = if buf[n - 1] == 0x00 { &buf[..n - 1] } else { &buf[..n] };
    serde_json::from_slice(trimmed).unwrap()
}

#[tokio::test]
async fn test_fd_server_create_and_unknown_action() {
    let dir = tempdir().unwrap();
    let sock_path = dir.path().join("fd_test.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();
    tokio::spawn(async move {
        let _ = run_fd_server(listener).await;
    });

    let mut client1 = UnixStream::connect(&sock_path).await.unwrap();
    write_framed(&mut client1, serde_json::json!({ "action": "invalid_cmd" })).await;
    let resp = read_framed_response(&mut client1).await;
    assert_eq!(resp.status, "error");
    assert!(resp.message.unwrap().contains("Unknown action"));

    let mut client2 = UnixStream::connect(&sock_path).await.unwrap();
    write_framed(
        &mut client2,
        serde_json::json!({ "action": "create", "name": "test_tensor", "size_bytes": 4096 }),
    )
    .await;
    client2.readable().await.unwrap();
    let mut buf = [0u8; 512];
    let (bytes, fd_opt) = recv_fd_from_unix(&client2, &mut buf).unwrap();
    assert!(bytes > 0);
    assert!(fd_opt.is_some(), "Client must receive sealed memfd via SCM_RIGHTS");
}

#[tokio::test]
async fn test_fd_server_rejects_oversize_request() {
    let dir = tempdir().unwrap();
    let sock_path = dir.path().join("oversize.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();
    tokio::spawn(async move {
        let _ = run_fd_server_with_quota(
            listener,
            Arc::new(FdQuota::new(MAX_MEMFD_BYTES * 4)),
        )
        .await;
    });

    let mut client = UnixStream::connect(&sock_path).await.unwrap();
    write_framed(
        &mut client,
        serde_json::json!({
            "action": "create",
            "size_bytes": MAX_MEMFD_BYTES + 1
        }),
    )
    .await;
    let resp = read_framed_response(&mut client).await;
    assert_eq!(resp.status, "error");
    assert!(resp.message.unwrap().contains("exceeds limit"));
}

#[tokio::test]
async fn test_fd_server_quota_enforced() {
    let dir = tempdir().unwrap();
    let sock_path = dir.path().join("quota.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();
    let quota = Arc::new(FdQuota::new(1024 * 1024));
    tokio::spawn(async move {
        let _ = run_fd_server_with_quota(listener, quota.clone()).await;
    });

    let mut c1 = UnixStream::connect(&sock_path).await.unwrap();
    write_framed(
        &mut c1,
        serde_json::json!({ "action": "create", "size_bytes": 768 * 1024 }),
    )
    .await;
    c1.readable().await.unwrap();
    let mut buf = [0u8; 512];
    let (_b, fd1) = recv_fd_from_unix(&c1, &mut buf).unwrap();
    assert!(fd1.is_some(), "first request fits in 1 MiB quota");

    let mut c2 = UnixStream::connect(&sock_path).await.unwrap();
    write_framed(
        &mut c2,
        serde_json::json!({ "action": "create", "size_bytes": 768 * 1024 }),
    )
    .await;
    c2.readable().await.unwrap();
    let mut buf2 = [0u8; 512];
    let (_b, fd2) = recv_fd_from_unix(&c2, &mut buf2).unwrap();
    assert!(fd2.is_some(), "second 768 KiB request fits: in-flight semantics release on success");

    let mut c3 = UnixStream::connect(&sock_path).await.unwrap();
    write_framed(
        &mut c3,
        serde_json::json!({ "action": "create", "size_bytes": 1024 * 1024 + 1 }),
    )
    .await;
    let resp = read_framed_response(&mut c3).await;
    assert_eq!(resp.status, "error");
    assert!(resp.message.unwrap().contains("Quota exceeded"));
}
