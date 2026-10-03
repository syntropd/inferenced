use crate::client::VarlinkClient;
use anyhow::Result;
use serde_json::json;
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;

/// Composable Unix stream filter:
/// Reads input prompt from stdin or arguments, dispatches inference over Varlink IPC
/// or offline mock stream filter fallback, and streams output tokens directly to stdout.
pub fn run_exec(
    socket_path: impl AsRef<Path>,
    model: &str,
    prompt_arg: Option<String>,
    quiet: bool,
) -> Result<()> {
    let mut prompt = String::new();

    if let Some(arg) = prompt_arg {
        prompt = arg;
    } else {
        let stdin = io::stdin();
        if !stdin.is_terminal() {
            let mut handle = stdin.lock().take(4 * 1024 * 1024);
            let _ = handle.read_to_string(&mut prompt);
        }
    }

    if prompt.trim().is_empty() {
        prompt = "Hello, systemd-inferenced!".into();
    }

    let mut stdout = io::stdout();

    // 1. Attempt live Varlink IPC streaming
    if let Ok(mut client) = VarlinkClient::connect(socket_path.as_ref()) {
        let stream_result = client.stream_call(
            "io.syntrop.Inference1.StreamInference",
            Some(json!({
                "model": model,
                "prompt": prompt,
            })),
            |msg| {
                if let Some(params) = msg.get("parameters") {
                    if let Some(chunk) = params.get("chunk").and_then(|v| v.as_str()) {
                        let _ = stdout.write_all(chunk.as_bytes());
                        let _ = stdout.flush();
                    }
                }
                Ok(())
            },
        );

        if stream_result.is_ok() {
            let _ = stdout.write_all(b"\n");
            let _ = stdout.flush();
            return Ok(());
        }
    }

    // 2. Offline mock fallback when daemon socket is not running
    if !quiet {
        eprintln!(
            "systemd-inferenced: daemon offline at {:?}, running local stream filter for model '{}'",
            socket_path.as_ref(),
            model
        );
    }

    let prompt_trimmed = prompt.trim();
    let tokens = [
        "Inference: ",
        "Analyzing prompt '",
        prompt_trimmed,
        "'. Execution dispatched via systemd-inferenced stream filter.",
    ];

    for token in &tokens {
        let _ = stdout.write_all(token.as_bytes());
        let _ = stdout.flush();
    }
    let _ = stdout.write_all(b"\n");
    let _ = stdout.flush();

    Ok(())
}
