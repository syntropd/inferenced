# Varlink IPC Specification: `io.systemd.inferenced1`

`systemd-inferenced` implements native Varlink IPC over a Unix domain socket:
`/run/systemd-inferenced/io.systemd.inferenced1`

Varlink protocol messages are UTF-8 JSON objects terminated by a single null byte (`\0`).

---

## 1. Interface Definition (IDL)

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

## 2. Standard Service Introspection (`org.varlink.service`)

The daemon implements the standard Varlink introspection interface:

```varlink
interface org.varlink.service

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
```

### Introspection Example
```bash
$ varlinkctl info unix:/run/systemd-inferenced/io.systemd.inferenced1
Vendor: systemd-inferenced
Product: systemd-inferenced
Version: 0.1.0
URL: https://github.com/syntropd/inferenced
Interfaces:
  org.varlink.service
  io.systemd.inferenced1
```

---

## 3. Method Specifications

### `GetTopology`
Returns discovered hardware compute planes (DRM GPU, NPU, UMA APU, CPU matrix extensions AMX/AVX-512) and system memory metrics.

### `GetPressure`
Returns Linux Pressure Stall Information (PSI) for CPU, memory, and I/O.
- `level`: `"Normal"`, `"Moderate"`, or `"Critical"`.

### `GetDrmWatermark`
Queries hardware VRAM telemetry and watermark saturation levels directly from DRM sysfs or memory subsystems.
- `device`: Optional filter targeting a specific DRM device name or path (e.g., `"renderD128"` or `"/dev/dri/renderD128"`). If omitted, returns watermarks for all discovered DRM devices.
- Returns `watermarks: []DrmWatermarkInfo`:
  - `device`: DRM render device path or identifier.
  - `vendor`: Hardware vendor string (`"amd"`, `"intel"`, `"nvidia"`, or `"unknown"`).
  - `vram_used`: Current VRAM consumed in bytes.
  - `vram_total`: Total physical VRAM capacity in bytes.
  - `used_percentage`: Fraction of VRAM currently occupied expressed as a percentage (`0.0`–`100.0`).
  - `fallback_psi`: `true` when physical DRM counters are unavailable and Linux Pressure Stall Information (PSI) heuristic memory metrics are substituted.

### `AcquireLease`
Acquires a resource slice on an accelerator plane.
- `priority`: `"EmergencyTriage"` (weight 100), `"Interactive"` (weight 10), or `"Batch"` (weight 0).
- `memory_bytes`: Requested allocation in bytes.
- `plane`: Target plane ID, or `None` for automatic arbitration.
- `unit`: Calling systemd cgroup unit name (e.g., `user@1000.service`).
- `pid`: Calling process ID for tracking.

### `ResizeLease`
Dynamically adjusts the memory allocation ceiling of an existing active compute lease without revoking or restarting the running client.
- `lease_id`: UUID string of the active compute lease to resize. Must match an active lease owned by the calling client connection.
- `memory_bytes`: New target memory allocation size in bytes (must be greater than 0).
- Returns:
  - `lease_id`: Confirmed lease UUID.
  - `plane_id`: Compute plane ID hosting the lease.
  - `allocated_memory`: New confirmed allocation in bytes.
- Errors:
  - `org.varlink.service.InvalidParameter`: Invalid UUID format, non-positive `memory_bytes`, or missing parameters.
  - `io.systemd.inferenced1.LeaseNotFound`: The specified lease UUID does not exist or is not associated with the calling client session.
  - `io.systemd.inferenced1.ResourceExhaustion`: Plane capacity is insufficient to accommodate the requested expansion.

### `ReleaseLease`
Releases an active lease, triggering memory reclamation or unfreezing queued batch workloads.

### `Yield`, `Freeze`, `Thaw`
Cooperative and kernel-enforced preemption controls:
- `Yield`: Emits a cooperative yield signal with a 250ms deadline.
- `Freeze`: Enforces preemption via `cgroup.freeze` or `SIGSTOP`.
- `Thaw`: Resumes a frozen workload.

### `StreamInference`
Streams generation output chunk-by-chunk using Varlink `continues: true` flags until the final token is sent (`continues: false`).

---

## 4. Error Mapping

| Varlink Error | HTTP / Exit Mapping | Meaning |
|---------------|---------------------|---------|
| `ResourceExhaustion` | 503 / 1 | Insufficient VRAM/RAM on plane |
| `PlaneNotFound` | 404 / 1 | Specified compute plane ID invalid |
| `LeaseNotFound` | 404 / 1 | Specified lease UUID does not exist |
| `ModelNotFound` | 404 / 1 | Model ID has not been registered |
| `InvalidParameter` | 400 / 1 | Malformed request parameter |

---

## 5. Command-Line Testing with `varlinkctl`

```bash
# Query interface description
varlinkctl introspect unix:/run/systemd-inferenced/io.systemd.inferenced1 io.systemd.inferenced1

# Call GetTopology
varlinkctl call unix:/run/systemd-inferenced/io.systemd.inferenced1 io.systemd.inferenced1.GetTopology '{}'

# Acquire an Interactive lease for 1GB
varlinkctl call unix:/run/systemd-inferenced/io.systemd.inferenced1 io.systemd.inferenced1.AcquireLease \
  '{"priority": "Interactive", "memory_bytes": 1073741824}'

# Query DRM VRAM watermarks across all accelerator devices
varlinkctl call unix:/run/systemd-inferenced/io.systemd.inferenced1 io.systemd.inferenced1.GetDrmWatermark '{}'

# Query DRM watermark for a specific device
varlinkctl call unix:/run/systemd-inferenced/io.systemd.inferenced1 io.systemd.inferenced1.GetDrmWatermark \
  '{"device": "renderD128"}'

# Dynamically resize an active lease to 2GB
varlinkctl call unix:/run/systemd-inferenced/io.systemd.inferenced1 io.systemd.inferenced1.ResizeLease \
  '{"lease_id": "c7a8b49e-1f23-4567-89ab-cdef01234567", "memory_bytes": 2147483648}'
```
