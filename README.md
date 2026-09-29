# systemd-inferenced

> **Unprivileged heterogeneous hardware arbiter and model lifecycle broker in pure Rust.**  
> Brings unified Linux resource scheduling, cgroups v2 isolation, and dynamic demand paging to local AI workloads across GPUs, NPUs, and CPU matrix planes.

[![Platform: Linux / systemd](https://img.shields.io/badge/Platform-Linux%20%2F%20systemd-red.svg)](https://systemd.io/)
[![Language: Rust](https://img.shields.io/badge/Language-Pure%20Rust-orange.svg)](https://www.rust-lang.org/)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](LICENSE)
[![Website](https://img.shields.io/badge/Website-syntropd.github.io-blue.svg)](https://syntropd.github.io/inferenced/)

> **Website & Documentation**: [https://syntropd.github.io/inferenced/](https://syntropd.github.io/inferenced/)

---

## The Problem: AI Workload Chaos on Linux

Modern AI inference engines (llama.cpp, vLLM, InvokeAI, Whisper) operate as unmanaged, greedy userland processes:
1. **Memory Contention & OOMs**: Runtimes pre-allocate entire VRAM or host memory pools up front, colliding with peer daemons and crashing with CUDA out-of-memory errors.
2. **Unified Memory Bus Starvation**: On integrated APUs and SoCs (AMD Ryzen AI, Intel Core Ultra, Apple/ARM), model execution consumes 80–120 GB/s across the shared memory bus, starving the desktop compositor, audio server, and core system daemons.
3. **Supervisory Blindness**: When an AI daemon locks up a GPU or host memory, system supervisory daemons like **[systemd-sentry](https://github.com/UberMetroid/systemd-sentry)** cannot diagnose the failure if they rely on the same hung compute plane.

---

## The Solution: systemd-inferenced

`systemd-inferenced` serves as an intelligent system gateway and resource broker:

* **Heterogeneous Hardware Topology**: Automatically enumerates discrete GPUs (`/dev/dri/renderD*`), NPUs (`/dev/accel/*`, Hailo, Coral), and CPU matrix extensions (Intel AMX, AVX-512 VNNI, ARM SME/SVE2).
* **Heterogeneous Speculative Scheduling**: Gang-schedules draft models (`PlaneRole::Draft`) onto energy-efficient CPU-host matrix extensions while assigning target models (`PlaneRole::Target`) to high-throughput discrete GPU/NPU planes.
* **Dynamic Demand Paging & Socket Activation**: Keeps heavy inference engines dormant (`InactiveExitStatus=0`) until an active socket connection requests a model lease.
* **cgroups v2 & PSI Enforcement**: Throttles or pauses batch workloads when Linux kernel Pressure Stall Information (PSI) signals bus or memory saturation.
* **`systemd-sentry` Emergency Triage Core**: Reserves and pins an out-of-band, low-latency compute slice (on NPU or dedicated CPU-AMX cores) specifically for `systemd-sentry`, ensuring crash diagnosis remains available even during discrete GPU driver deadlocks.
* **Companion CLI (`inferenctl`)**: Provides operators with real-time insight into compute plane utilization, memory pressure, active leases, and model residency.

---

## Workspace Architecture

```
systemd-inferenced/
├── Cargo.toml
├── ARCHITECTURE.md
├── crates/
│   ├── inferenced-core/      # Hardware discovery, leases, topology & scheduler
│   ├── inferenced-daemon/    # systemd-inferenced daemon (Varlink, D-Bus, Gateway)
│   └── inferenctl/           # System administrator CLI utility
└── systemd/                  # Unit files, slices, and configuration
    ├── systemd-inferenced.service
    ├── systemd-inferenced.socket
    ├── ai.slice
    └── inferenced.conf
```

---

## CLI Quick Inspection (`inferenctl`)

```bash
# Display detected compute planes, pressure metrics, and active leases
inferenctl status

# List registered models and residency state
inferenctl models

# Pin an emergency model for systemd-sentry triage
inferenctl pin llama3.2:1b --plane npu-accel0
```

---

## License

Licensed under [Apache License, Version 2.0](LICENSE).
