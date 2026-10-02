//! Handler for io.syntrop.Telemetry1 Varlink methods.

use crate::varlink::protocol::VarlinkReply;
use inferenced_core::psi::PressureMetrics;
use serde_json::json;

/// Dispatches method calls for io.syntrop.Telemetry1.
pub fn handle_telemetry_method(method: &str) -> Option<VarlinkReply> {
    let sub = method.strip_prefix("io.syntrop.Telemetry1.")?;
    match sub {
        "GetKernelPressure" => Some(handle_get_kernel_pressure()),
        _ => None,
    }
}

fn handle_get_kernel_pressure() -> VarlinkReply {
    let psi = PressureMetrics::read_current();
    VarlinkReply::ok(json!({
        "memory_some": psi.memory_some_avg10,
        "memory_full": psi.memory_full_avg10,
        "cpu_some": psi.cpu_some_avg10,
        "io_some": psi.io_some_avg10,
        "runqueue_latency_us": psi.runqueue_latency_us,
        "ebpf_active": psi.ebpf_active,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_telemetry_method() {
        let reply = handle_telemetry_method("io.syntrop.Telemetry1.GetKernelPressure");
        assert!(reply.is_some());
    }
}
