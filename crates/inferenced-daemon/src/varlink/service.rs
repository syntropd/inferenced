use super::interface_descriptions::{
    IO_SYNTROP_INFERENCE1_IDL, IO_SYSTEMD_INFERENCED1_IDL, ORG_VARLINK_SERVICE_IDL,
};
use super::protocol::VarlinkReply;
use serde_json::{json, Value};

pub fn handle_get_info() -> VarlinkReply {
    VarlinkReply::ok(json!({
        "vendor": "Syntropd Project",
        "product": "inferenced",
        "version": env!("CARGO_PKG_VERSION"),
        "url": "https://github.com/syntropd/inferenced",
        "interfaces": [
            "org.varlink.service",
            "io.syntrop.Inference1",
            "io.systemd.inferenced1",
            "io.syntrop.Telemetry1"
        ]
    }))
}

pub fn handle_get_interface_description(params: Option<&Value>) -> VarlinkReply {
    let iface = params
        .and_then(|p| p.get("interface"))
        .and_then(|v| v.as_str());

    match iface {
        Some("org.varlink.service") => VarlinkReply::ok(json!({
            "description": ORG_VARLINK_SERVICE_IDL
        })),
        Some("io.syntrop.Inference1") => VarlinkReply::ok(json!({
            "description": IO_SYNTROP_INFERENCE1_IDL
        })),
        Some("io.systemd.inferenced1") => VarlinkReply::ok(json!({
            "description": IO_SYSTEMD_INFERENCED1_IDL
        })),
        Some("io.syntrop.Telemetry1") => VarlinkReply::ok(json!({
            "description": super::telemetry::IO_SYNTROP_TELEMETRY1_IDL
        })),
        Some(unknown) => VarlinkReply::error(
            "org.varlink.service.InterfaceNotFound",
            json!({ "interface": unknown }),
        ),
        None => VarlinkReply::error(
            "org.varlink.service.InvalidParameter",
            json!({ "parameter": "interface" }),
        ),
    }
}
