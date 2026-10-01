# Varlink IPC Protocol Specification: `io.systemd.inferenced1`

`systemd-inferenced` exposes a pure Rust Varlink IPC interface over the local Unix domain socket:
`/run/systemd-inferenced/io.systemd.inferenced1`

This interface replaces legacy D-Bus overhead with zero-dependency, NUL-delimited JSON streaming.

---

## 1. Introspection & Standard Interfaces

The service implements standard `org.varlink.service`:

```bash
# Query service information
varlinkctl info unix:/run/systemd-inferenced/io.systemd.inferenced1

# Inspect the inferenced1 interface definition
varlinkctl introspect unix:/run/systemd-inferenced/io.systemd.inferenced1 io.systemd.inferenced1
```

---

## 2. Interface Definition Language (IDL)

```varlink
interface io.systemd.inferenced1

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

type DrmWatermarkInfo (
  device: string,
  vendor: string,
  vram_used: int,
  vram_total: int,
  used_percentage: float,
  fallback_psi: bool
)

method GetDrmWatermark(device: ?string) -> (watermarks: []DrmWatermarkInfo)
method ResizeLease(lease_id: string, memory_bytes: int) -> (lease_id: string, plane_id: string, allocated_memory: int)

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
```

---

## 3. CLI Invocation Examples

### Query Hardware Topology
```bash
varlinkctl call unix:/run/systemd-inferenced/io.systemd.inferenced1 \
  io.systemd.inferenced1.GetTopology
```

### Query Linux Kernel PSI Pressure
```bash
varlinkctl call unix:/run/systemd-inferenced/io.systemd.inferenced1 \
  io.systemd.inferenced1.GetPressure
```

### Acquire a Compute Lease
```bash
varlinkctl call unix:/run/systemd-inferenced/io.systemd.inferenced1 \
  io.systemd.inferenced1.AcquireLease \
  '{"priority":"Interactive","memory_bytes":2147483648,"unit":"my-inference.service"}'
```

### Dynamically Resize a Compute Lease
```bash
varlinkctl call unix:/run/systemd-inferenced/io.systemd.inferenced1 \
  io.systemd.inferenced1.ResizeLease \
  '{"lease_id":"c7a8b49e-1f23-4567-89ab-cdef01234567","memory_bytes":4294967296}'
```

### Query DRM VRAM Telemetry & Watermarks
```bash
varlinkctl call unix:/run/systemd-inferenced/io.systemd.inferenced1 \
  io.systemd.inferenced1.GetDrmWatermark '{}'

# Query watermark for a specific device
varlinkctl call unix:/run/systemd-inferenced/io.systemd.inferenced1 \
  io.systemd.inferenced1.GetDrmWatermark '{"device":"renderD128"}'
```

### Stream Real-Time Inference
```bash
varlinkctl call --more unix:/run/systemd-inferenced/io.systemd.inferenced1 \
  io.systemd.inferenced1.StreamInference \
  '{"model":"qwen2.5-coder:7b","prompt":"Explain cgroups v2 memory.min"}'
```
