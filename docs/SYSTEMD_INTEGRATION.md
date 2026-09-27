# systemd Integration Guide for systemd-inferenced

`systemd-inferenced` is designed for zero-trust Linux environments, integrating natively with systemd primitives in 100% pure Rust without linking to `libsystemd.so` or `libdbus-1.so`.

---

## 1. Slices & cgroups v2 Resource Hierarchy

All inference tasks and daemon processes run under the `ai.slice` hierarchy:

```text
/sys/fs/cgroup/ai.slice
├── systemd-inferenced.service # Daemon lifecycle broker (ManagedOOMPreference=avoid)
├── ai-sentry.slice            # Out-of-band triage enclave
│   ├── MemoryMin=2G           # Guaranteed locked host RAM
│   ├── CPUWeight=1000         # Maximum scheduling weight
│   ├── ManagedOOMPreference=avoid
│   └── ManagedOOMMemoryPressureLimit=95%
├── ai-interactive.slice       # Low-latency interactive chat / tool-use
│   ├── CPUWeight=500
│   └── MemoryHigh=70%
└── ai-batch.slice             # Offline background batch embeddings / diffusion
    ├── CPUWeight=50
    └── MemoryHigh=50%
```

---

## 2. Pure Rust Socket Activation (`$LISTEN_FDS`)

The daemon uses pure Rust (`rustix`) to adopt pre-bound file descriptors passed by systemd at startup:

| File Descriptor | Socket Type | Path / Address | Purpose |
|-----------------|-------------|----------------|---------|
| `FD 3` | `AF_UNIX` Stream | `/run/systemd-inferenced/io.systemd.inferenced1` | Native Varlink IPC |
| `FD 4` | `AF_UNIX` Stream | `/run/systemd-inferenced/sentry.sock` | Sentry Emergency Triage |
| `FD 5` | `AF_UNIX` Stream | `/run/syntrop/gateway.sock` | OpenAI-compatible HTTP Gateway |

This ensures the daemon starts instantaneously on incoming requests with zero port collisions.

---

## 3. Abstract Linux Socket Notifications (`sd_notify`)

`systemd-inferenced` signals daemon readiness and health directly over `$NOTIFY_SOCKET`:
- `READY=1`: Emitted once compute planes are enumerated and listeners active.
- `STATUS=...`: Operational heartbeat reporting active leases and memory load.
- `STOPPING=1`: Emitted during graceful SIGINT/SIGTERM shutdown.

All notification packets are written via datagram send to Linux abstract sockets (`@...`) with zero external C dependencies.

---

## 4. Security Sandboxing & Least Privilege

The daemon runs under an unprivileged user (`inferenced:inferenced`) with extensive sandboxing:

* **Filesystem Isolation**:
  - `ProtectSystem=strict`: The entire host filesystem is mounted read-only.
  - `ProtectHome=yes`: `/home`, `/root`, and `/run/user` are inaccessible.
  - `PrivateTmp=yes`: Isolated `/tmp` and `/var/tmp` namespaces.
  - `RuntimeDirectory=systemd-inferenced`: Manages `/run/systemd-inferenced/`.
  - `StateDirectory=systemd-inferenced`: Manages `/var/lib/systemd-inferenced/`.
* **Kernel & Memory Protection**:
  - `NoNewPrivileges=yes`: Prevents privilege escalation.
  - `MemoryDenyWriteExecute=yes`: Prohibits creating executable memory pages.
  - `ProtectKernelModules=yes` & `ProtectKernelTunables=yes`: Locks kernel knobs.
  - `ProtectControlGroups=yes`: Mounts cgroup controllers read-only.
  - `LockPersonality=yes`: Disables legacy execution domain emulation.
* **Device Access Whitelisting**:
  - `DeviceAllow=/dev/dri/renderD* rw`: Discrete and integrated GPU compute.
  - `DeviceAllow=/dev/accel/* rw`: Linux NPU subsystem devices.
  - `DeviceAllow=/dev/hailo* rw`: Hailo NPU acceleration devices.
  - `DeviceAllow=/dev/kfd rw`: AMD ROCm Kernel Fusion Driver.
* **OOM Killer Immunity**:
  - `ManagedOOMPreference=avoid`: Ensures `systemd-oomd` sacrifices user-space batch tasks before terminating the hardware broker.

---

## 5. Operations & Troubleshooting

```bash
# Verify systemd units syntax
systemd-analyze verify /usr/lib/systemd/system/systemd-inferenced.*

# Check socket activation status
systemctl status systemd-inferenced.socket

# Inspect security sandboxing score
systemd-analyze security systemd-inferenced.service

# View daemon journal logs
journalctl -u systemd-inferenced.service -f
```
