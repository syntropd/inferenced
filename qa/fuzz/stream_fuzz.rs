use serde_json::json;

#[test]
fn test_fuzz_stream_tokens_with_control_characters() {
    let raw_tokens = [
        "\x1b[31;1mANSI_RED\x1b[0m",
        "\r\n\t\x08\x07",
        "Unicode: 🦀 Rust \u{1F916} AI \u{2728} Sparkles",
        "Control \x01\x02\x03\x04\x05\x06",
        "Zero-Width \u{200B}\u{200C}\u{200D}\u{FEFF}",
    ];

    for token in raw_tokens {
        let chunk_json = json!({
            "parameters": {
                "chunk": token
            },
            "continues": true
        });
        let bytes = serde_json::to_vec(&chunk_json).unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed["parameters"]["chunk"], token);
    }
}

#[test]
fn test_fuzz_stream_tokens_with_embedded_nul_byte() {
    // In Varlink wire protocol, messages are delimited by 0 byte (NUL)
    // If a token string contains an escaped NUL ("\u0000"), JSON handles it cleanly
    let json_str = "{\"parameters\":{\"chunk\":\"hello\\u0000world\"},\"continues\":true}";
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(json_str);
    assert!(parsed.is_ok(), "Escaped NUL inside JSON string must parse validly");
}

#[test]
fn test_fuzz_stream_huge_prompt_serialization() {
    let mut huge_prompt = String::with_capacity(500_000);
    for _ in 0..50_000 {
        huge_prompt.push_str("tokenize ");
    }

    let req = json!({
        "method": "io.syntrop.Inference1.StreamInference",
        "parameters": {
            "model": "qwen2.5-coder:7b",
            "prompt": huge_prompt
        }
    });

    let serialized = serde_json::to_vec(&req).unwrap();
    assert!(serialized.len() >= 400_000);

    let parsed: serde_json::Value = serde_json::from_slice(&serialized).unwrap();
    assert_eq!(parsed["method"], "io.syntrop.Inference1.StreamInference");
}

#[test]
fn test_fuzz_stream_malformed_chunk_envelopes() {
    let malformed_chunks = [
        "{\"parameters\": null}",
        "{\"parameters\": {}}",
        "{\"parameters\": {\"chunk\": 12345}}",
        "{\"parameters\": {\"chunk\": [\"array\"]}}",
        "{\"continues\": \"not_a_bool\"}",
    ];

    for raw in malformed_chunks {
        let res: Result<serde_json::Value, _> = serde_json::from_str(raw);
        assert!(res.is_ok()); // Valid JSON syntax, but schema-divergent
        let val = res.unwrap();
        // Safe access must not panic
        let _chunk = val.get("parameters")
            .and_then(|p| p.get("chunk"))
            .and_then(|c| c.as_str());
    }
}

#[test]
fn test_fuzz_stream_empty_and_whitespace_prompts() {
    let empty_variants = ["", "   ", "\t\t", "\n\n\r\n", " \t \r \n "];
    for p in empty_variants {
        let prompt_trimmed = p.trim();
        let prompt_final = if prompt_trimmed.is_empty() {
            "Hello, systemd-inferenced!"
        } else {
            prompt_trimmed
        };
        assert_eq!(prompt_final, "Hello, systemd-inferenced!");
    }
}
