use serde::Deserialize;
use serde_json::Value;

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct VarlinkRequest {
    pub method: String,
    #[serde(default)]
    pub parameters: Option<Value>,
    #[serde(default)]
    pub more: Option<bool>,
    #[serde(default)]
    pub oneway: Option<bool>,
}

fn parse_varlink_frame(bytes: &[u8]) -> Result<VarlinkRequest, String> {
    let clean = if let Some(&0) = bytes.last() {
        &bytes[..bytes.len() - 1]
    } else {
        bytes
    };
    let req: VarlinkRequest =
        serde_json::from_slice(clean).map_err(|e| format!("Varlink framing error: {}", e))?;
    if req.method.is_empty() {
        return Err("Empty method name".into());
    }
    Ok(req)
}

#[test]
fn test_fuzz_varlink_random_garbage() {
    let mut rng_seed = 0xdeadbeef_u64;
    let mut pseudo_rand = || {
        rng_seed = rng_seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (rng_seed >> 32) as u8
    };

    for len in 1..500 {
        let mut junk = Vec::with_capacity(len);
        for _ in 0..len {
            junk.push(pseudo_rand());
        }
        junk.push(0);
        let _ = parse_varlink_frame(&junk);
    }
}

#[test]
fn test_fuzz_varlink_malformed_json_patterns() {
    let corrupted_samples = [
        "",
        "\0",
        "\0\0\0\0",
        "{",
        "}",
        "{\"method\":",
        "{\"method\": \"foo",
        "{\"method\": null}",
        "{\"method\": 12345}",
        "{\"parameters\": [}",
        "{\"method\": \"test\", \"more\": \"not-a-bool\"}",
        "{\"method\": \"\"}",
        "[[[[[[[[[[",
    ];

    for sample in corrupted_samples {
        let mut msg = sample.as_bytes().to_vec();
        msg.push(0);
        let res = parse_varlink_frame(&msg);
        assert!(
            res.is_err(),
            "Sample {:?} should fail Varlink schema validation",
            sample
        );
    }
}

#[test]
fn test_fuzz_varlink_valid_framing() {
    let valid = b"{\"method\":\"io.syntrop.Inference1.GetTopology\",\"parameters\":{}}\0";
    let parsed = parse_varlink_frame(valid).expect("Valid Varlink frame must parse cleanly");
    assert_eq!(parsed.method, "io.syntrop.Inference1.GetTopology");
}

#[test]
fn test_fuzz_varlink_huge_message_resilience() {
    let huge_string = "A".repeat(100_000);
    let json_msg = format!(
        "{{\"method\":\"test\",\"parameters\":{{\"data\":\"{}\"}}}}\0",
        huge_string
    );
    let res = parse_varlink_frame(json_msg.as_bytes());
    assert!(
        res.is_ok(),
        "Large well-formed Varlink message should parse without issue"
    );
}
