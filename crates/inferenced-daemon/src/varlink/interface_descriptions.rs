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

pub const IO_SYNTROP_INFERENCE1_IDL: &str = r#"interface io.syntrop.Inference1

type ComputePlane (
  id: string,
  name: string,
  kind: string,
  total_memory: int,
  available_memory: int,
  is_triage_reserved: bool
)

type LeaseInfo (
  id: string,
  plane_id: string,
  allocated_memory: int,
  priority: string,
  state: string,
  client_unit: ?string,
  client_pid: ?int
)

type ModelInfo (
  id: string,
  format: string,
  path: string,
  estimated_memory: int,
  placement: string
)

method GetStatus() -> (
  status: string,
  version: string,
  daemon: string,
  planes_count: int,
  leases_count: int,
  pressure: string
)

method ListPlanes() -> (
  planes: []ComputePlane
)

method GetTopology() -> (
  planes: []ComputePlane,
  total_ram: int,
  available_ram: int,
  cpu_cores: int
)

method GetPressure() -> (
  level: string,
  cpu_some: float,
  memory_some: float,
  io_some: float
)

method AcquireLease(
  priority: string,
  memory_bytes: int,
  plane: ?string,
  unit: ?string,
  pid: ?int
) -> (
  lease_id: string,
  plane_id: string,
  allocated_memory: int
)

type SliceReq (role: string, memory_bytes: int, plane: ?string)
type SliceAlloc (plane_id: string, role: string, allocated_memory: int, numa_node: ?int)
method AcquireCompositeLease(priority: string, policy: ?string, slices: []SliceReq, unit: ?string, pid: ?int) -> (lease_id: string, slices: []SliceAlloc, priority: string, policy: string)
method ReleaseCompositeLease(lease_id: string) -> ()
method ReleaseLease(lease_id: string) -> ()
method Yield(lease_id: string) -> ()
method Freeze(lease_id: string) -> ()
method Thaw(lease_id: string) -> ()
method ListLeases() -> (leases: []LeaseInfo)
method ListModels() -> (models: []ModelInfo)

method RegisterModel(
  id: string,
  format: string,
  path: string,
  estimated_bytes: int
) -> ()

method EvictModel(model: string) -> ()
method PinModel(model: string, plane: string) -> ()

method StreamInference(
  model: string,
  prompt: string
) -> (
  chunk: string
)

error MethodNotFound (method: string)
error InvalidParameter (parameter: string)
error ResourceExhaustion (plane: string, requested: int, available: int)
error PlaneNotFound (plane: string)
error LeaseNotFound (lease_id: string)
error ModelNotFound (model: string)
"#;

pub const IO_SYSTEMD_INFERENCED1_IDL: &str = r#"interface io.systemd.inferenced1

type ComputePlane (
  id: string,
  name: string,
  kind: string,
  total_memory: int,
  available_memory: int,
  is_triage_reserved: bool
)

type LeaseInfo (
  id: string,
  plane_id: string,
  allocated_memory: int,
  priority: string,
  state: string,
  client_unit: ?string,
  client_pid: ?int
)

type ModelInfo (
  id: string,
  format: string,
  path: string,
  estimated_memory: int,
  placement: string
)

method GetStatus() -> (
  status: string,
  version: string,
  daemon: string,
  planes_count: int,
  leases_count: int,
  pressure: string
)

method ListPlanes() -> (
  planes: []ComputePlane
)

method GetTopology() -> (
  planes: []ComputePlane,
  total_ram: int,
  available_ram: int,
  cpu_cores: int
)

method GetPressure() -> (
  level: string,
  cpu_some: float,
  memory_some: float,
  io_some: float
)

method AcquireLease(
  priority: string,
  memory_bytes: int,
  plane: ?string,
  unit: ?string,
  pid: ?int
) -> (
  lease_id: string,
  plane_id: string,
  allocated_memory: int
)

type SliceReq (role: string, memory_bytes: int, plane: ?string)
type SliceAlloc (plane_id: string, role: string, allocated_memory: int, numa_node: ?int)
method AcquireCompositeLease(priority: string, policy: ?string, slices: []SliceReq, unit: ?string, pid: ?int) -> (lease_id: string, slices: []SliceAlloc, priority: string, policy: string)
method ReleaseCompositeLease(lease_id: string) -> ()
method ReleaseLease(lease_id: string) -> ()
method Yield(lease_id: string) -> ()
method Freeze(lease_id: string) -> ()
method Thaw(lease_id: string) -> ()
method ListLeases() -> (leases: []LeaseInfo)
method ListModels() -> (models: []ModelInfo)

method RegisterModel(
  id: string,
  format: string,
  path: string,
  estimated_bytes: int
) -> ()

method EvictModel(model: string) -> ()
method PinModel(model: string, plane: string) -> ()

method StreamInference(
  model: string,
  prompt: string
) -> (
  chunk: string
)

error MethodNotFound (method: string)
error InvalidParameter (parameter: string)
error ResourceExhaustion (plane: string, requested: int, available: int)
error PlaneNotFound (plane: string)
error LeaseNotFound (lease_id: string)
error ModelNotFound (model: string)
"#;

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
