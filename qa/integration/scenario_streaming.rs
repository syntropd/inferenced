use inferenced_core::{
    arbiter::Arbiter,
    fd_lease::{create_sealed_memfd, recv_fd_from_unix, send_fd_over_unix},
    lease::LeasePriority,
    topology::{ComputePlane, HardwareTopology},
};
use rustix::fs::{seek, SeekFrom};
use rustix::io::read;
use serde_json::json;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

#[tokio::test]
async fn test_scenario_high_volume_token_streaming() {
    // Scenario 1: Composable Unix stream filter with zero-copy memfd weight distribution
    let dir = tempdir().unwrap();
    let socket_path = dir.path().join("inferenced_stream.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();

    let mut topo = HardwareTopology::default();
    topo.planes.push(
        ComputePlane::builder("stream-gpu-0")
            .name("Streaming GPU")
            .no_device_path()
            .total_memory(16 * 1024 * 1024 * 1024)
            .numa_node(None)
            .supported_formats(vec![])
            .hardware_features(vec![])
            .build(),
    );

    let arbiter = Arc::new(Arbiter::new(topo));
    let server_arbiter = arbiter.clone();

    // Spawn daemon stream endpoint
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut buf_reader = BufReader::new(reader);
        let mut line = Vec::new();

        buf_reader.read_until(0, &mut line).await.unwrap();
        let req: serde_json::Value = serde_json::from_slice(&line[..line.len() - 1]).unwrap();
        assert_eq!(req["method"], "io.syntrop.Inference1.StreamInference");

        let prompt = req["parameters"]["prompt"].as_str().unwrap();

        let _lease = server_arbiter
            .acquire_lease(LeasePriority::Interactive, 2 * 1024 * 1024 * 1024, None, None, None)
            .await
            .unwrap();

        let tokens = [
            "Generated: ",
            "Token1 ",
            "Token2 ",
            "from prompt '",
            prompt,
            "'.",
        ];

        for (i, token) in tokens.iter().enumerate() {
            let continues = i < tokens.len() - 1;
            let chunk_reply = json!({
                "parameters": { "chunk": token },
                "continues": continues
            });
            let mut b = serde_json::to_vec(&chunk_reply).unwrap();
            b.push(0);
            writer.write_all(&b).await.unwrap();
        }
    });

    // 1. Zero-copy model weight preparation
    let model_weights = vec![0xABu8; 64 * 1024]; // 64 KB test weights
    let weight_fd = create_sealed_memfd("stream_model", model_weights.len() as u64, Some(&model_weights)).unwrap();

    let (s1, s2) = UnixStream::pair().unwrap();
    send_fd_over_unix(&s1, &weight_fd, b"model-qwen").unwrap();

    let mut fd_buf = [0u8; 32];
    let (_, received_fd_opt) = recv_fd_from_unix(&s2, &mut fd_buf).unwrap();
    let received_weight_fd = received_fd_opt.expect("Should receive weight fd");

    seek(&received_weight_fd, SeekFrom::Start(0)).unwrap();
    let mut read_weights = vec![0u8; model_weights.len()];
    read(&received_weight_fd, &mut read_weights).unwrap();
    assert_eq!(&read_weights, &model_weights);

    // 2. Client connects and streams tokens line by line
    let client_stream = tokio::net::UnixStream::connect(&socket_path).await.unwrap();
    let (reader, mut writer) = client_stream.into_split();
    let mut buf_reader = BufReader::new(reader);

    let req = json!({
        "method": "io.syntrop.Inference1.StreamInference",
        "parameters": {
            "model": "qwen2.5-coder:7b",
            "prompt": "Explain Linux demand paging"
        },
        "more": true
    });
    let mut req_bytes = serde_json::to_vec(&req).unwrap();
    req_bytes.push(0);
    writer.write_all(&req_bytes).await.unwrap();

    let mut stream_output = String::new();
    let mut chunk_buf = Vec::new();

    loop {
        chunk_buf.clear();
        let n = buf_reader.read_until(0, &mut chunk_buf).await.unwrap();
        if n == 0 { break; }
        let reply: serde_json::Value = serde_json::from_slice(&chunk_buf[..chunk_buf.len() - 1]).unwrap();
        if let Some(chunk) = reply["parameters"]["chunk"].as_str() {
            stream_output.push_str(chunk);
        }
        let continues = reply["continues"].as_bool().unwrap_or(false);
        if !continues { break; }
    }

    assert!(stream_output.contains("Generated:"));
    assert!(stream_output.contains("Explain Linux demand paging"));
}
