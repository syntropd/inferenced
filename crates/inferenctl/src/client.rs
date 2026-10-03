use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

#[allow(dead_code)]
pub const DEFAULT_SOCKET_PATH: &str = "/run/syntrop/io.syntrop.Inference1";

pub struct VarlinkClient {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl VarlinkClient {
    pub fn connect(socket_path: impl AsRef<Path>) -> Result<Self> {
        let stream = UnixStream::connect(socket_path.as_ref()).with_context(|| {
            format!("Failed to connect to inferenced at {:?}", socket_path.as_ref())
        })?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self { stream, reader })
    }

    pub fn call(&mut self, method: &str, parameters: Option<Value>) -> Result<Value> {
        let req = json!({ "method": method, "parameters": parameters.unwrap_or_else(|| json!({})) });
        let mut msg = serde_json::to_vec(&req)?;
        msg.push(0);
        self.stream.write_all(&msg)?;
        self.stream.flush()?;

        let mut buf = Vec::new();
        (&mut self.reader).take(1024 * 1024).read_until(0, &mut buf)?;
        if buf.is_empty() {
            bail!("Connection closed by inferenced daemon");
        }
        if let Some(&0) = buf.last() {
            buf.pop();
        } else {
            bail!("Varlink framing error: message exceeds 1MB framing limit or missing NUL delimiter");
        }

        let resp: Value = serde_json::from_slice(&buf)?;
        if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
            bail!("Varlink error {}: {:?}", err, resp.get("parameters"));
        }
        Ok(resp.get("parameters").cloned().unwrap_or_else(|| json!({})))
    }

    pub fn stream_call<F: FnMut(&Value) -> Result<()>>(
        &mut self,
        method: &str,
        parameters: Option<Value>,
        mut on_message: F,
    ) -> Result<()> {
        let req = json!({
            "method": method,
            "parameters": parameters.unwrap_or_else(|| json!({})),
            "more": true,
        });
        let mut msg = serde_json::to_vec(&req)?;
        msg.push(0);
        self.stream.write_all(&msg)?;
        self.stream.flush()?;

        let mut buf = Vec::new();
        loop {
            buf.clear();
            let n = (&mut self.reader).take(1024 * 1024).read_until(0, &mut buf)?;
            if n == 0 {
                break;
            }
            if let Some(&0) = buf.last() {
                buf.pop();
            } else {
                bail!("Varlink stream framing error: message exceeds 1MB framing limit or missing NUL delimiter");
            }
            let resp: Value = serde_json::from_slice(&buf)?;
            if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
                bail!("Varlink stream error {}: {:?}", err, resp.get("parameters"));
            }
            on_message(&resp)?;
            let continues = resp.get("continues").and_then(|v| v.as_bool()).unwrap_or(false);
            if !continues {
                break;
            }
        }
        Ok(())
    }

    pub fn get_topology(&mut self) -> Result<Value> {
        self.call("io.syntrop.Inference1.GetTopology", None)
    }

    pub fn get_pressure(&mut self) -> Result<Value> {
        self.call("io.syntrop.Inference1.GetPressure", None)
    }

    pub fn list_leases(&mut self) -> Result<Value> {
        self.call("io.syntrop.Inference1.ListLeases", None)
    }

    pub fn acquire_lease(&mut self, priority: &str, memory_bytes: u64, plane: Option<&str>) -> Result<Value> {
        self.call("io.syntrop.Inference1.AcquireLease", Some(json!({
            "priority": priority, "memory_bytes": memory_bytes, "plane": plane,
        })))
    }

    pub fn release_lease(&mut self, lease_id: &str) -> Result<Value> {
        self.call("io.syntrop.Inference1.ReleaseLease", Some(json!({ "lease_id": lease_id })))
    }

    pub fn freeze(&mut self, lease_id: &str) -> Result<Value> {
        self.call("io.syntrop.Inference1.Freeze", Some(json!({ "lease_id": lease_id })))
    }

    pub fn thaw(&mut self, lease_id: &str) -> Result<Value> {
        self.call("io.syntrop.Inference1.Thaw", Some(json!({ "lease_id": lease_id })))
    }

    pub fn list_models(&mut self) -> Result<Value> {
        self.call("io.syntrop.Inference1.ListModels", None)
    }

    pub fn register_model(&mut self, id: &str, format: &str, path: &str, bytes: u64) -> Result<Value> {
        self.call("io.syntrop.Inference1.RegisterModel", Some(json!({
            "id": id, "format": format, "path": path, "estimated_bytes": bytes,
        })))
    }

    pub fn evict_model(&mut self, model: &str) -> Result<Value> {
        self.call("io.syntrop.Inference1.EvictModel", Some(json!({ "model": model })))
    }

    pub fn pin_model(&mut self, model: &str, plane: &str) -> Result<Value> {
        self.call("io.syntrop.Inference1.PinModel", Some(json!({ "model": model, "plane": plane })))
    }

    pub fn get_info(&mut self) -> Result<Value> {
        self.call("org.varlink.service.GetInfo", None)
    }

    #[allow(dead_code)]
    pub fn get_interface_description(&mut self, interface: &str) -> Result<Value> {
        self.call("org.varlink.service.GetInterfaceDescription", Some(json!({ "interface": interface })))
    }
}
