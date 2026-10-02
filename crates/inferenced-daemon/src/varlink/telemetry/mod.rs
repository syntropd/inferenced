//! io.syntrop.Telemetry1 Varlink interface definitions and dispatchers.

pub mod interface;
pub mod telemetry1;

pub use interface::IO_SYNTROP_TELEMETRY1_IDL;
pub use telemetry1::handle_telemetry_method;
