//! Served Varlink interface descriptions (IDL documents).
//!
//! Returned verbatim by `org.varlink.service.GetInterfaceDescription`.

pub const ORG_VARLINK_SERVICE_IDL: &str = r#"interface org.varlink.service

method GetInfo() -> (
  vendor: string,
  product: string,
  version: string,
  url: string,
  interfaces: []string
)

method GetInterfaceDescription(interface: string) -> (description: string)

error InterfaceNotFound (interface: string)
error MethodNotFound (method: string)
error MethodNotImplemented (method: string)
error InvalidParameter (parameter: string)
"#;

pub const IO_SYNTROP_INFERENCE1_IDL: &str = include_str!("idl/io.syntrop.Inference1.varlink");

pub const IO_SYSTEMD_INFERENCED1_IDL: &str = include_str!("idl/io.systemd.inferenced1.varlink");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idl_names_its_interface() {
        assert!(ORG_VARLINK_SERVICE_IDL.contains("interface org.varlink.service"));
        assert!(IO_SYNTROP_INFERENCE1_IDL.contains("interface io.syntrop.Inference1"));
        assert!(IO_SYNTROP_INFERENCE1_IDL.contains("AcquireCompositeLease"));
        assert!(IO_SYSTEMD_INFERENCED1_IDL.contains("interface io.systemd.inferenced1"));
        assert!(IO_SYSTEMD_INFERENCED1_IDL.contains("AcquireCompositeLease"));
    }
}
